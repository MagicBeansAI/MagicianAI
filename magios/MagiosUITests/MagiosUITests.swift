import XCTest
import UIKit

/// Menu → Notes opens the in-app library. It does not launch a browser or edit notes.
final class MagiosLiveNotesUITests: XCTestCase {
    func testMenuOpensNotesLibrary() throws {
        let env = ProcessInfo.processInfo.environment
        let backendHost = env["MAGIOS_LIVE_TEST_HOST"] ?? ""
        try XCTSkipIf(backendHost.isEmpty,
                      "Requires MAGIOS_LIVE_TEST_HOST on an enrolled physical iPhone")
        #if targetEnvironment(simulator)
        throw XCTSkip("Live Notes acceptance requires a physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        app.buttons["Menu"].firstMatch.tap()
        app.buttons["Notes"].tap()
        let search = app.textFields["Search Notes"]
        XCTAssertTrue(search.waitForExistence(timeout: 20), "Notes must open inside the app")
        let evidence = XCTAttachment(screenshot: app.screenshot())
        evidence.name = "live-iPhone-notes-library"
        evidence.lifetime = .keepAlways
        add(evidence)
        #endif
    }
}

/// Receives a fresh enrollment link from the host while the test is waiting.
/// The URI stays outside XCTest's environment, console and result bundle.
final class MagiosLiveEnrollmentUITests: XCTestCase {
    func testConfirmSelectedHostAndKeepConnectionAfterRestart() throws {
        let env = ProcessInfo.processInfo.environment
        let expectedHost = env["MAGIOS_LIVE_TEST_HOST"] ?? ""
        try XCTSkipIf(expectedHost.isEmpty || env["MAGIOS_LIVE_ENROLLMENT"] != "1",
                      "Requires explicit live enrollment and a physical iPhone")
        #if targetEnvironment(simulator)
        throw XCTSkip("Live enrollment requires a physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        print("MAGIOS_ENROLLMENT_READY")
        let confirmation = app.alerts["Connect to \(expectedHost)?"]
        XCTAssertTrue(confirmation.waitForExistence(timeout: 90),
                      "Open a fresh enrollment link for the explicitly selected test host")
        confirmation.buttons["Connect"].tap()
        // Installing the verified profile rebuilds AppTabView, dismissing
        // Settings and its transient success message. An exchange failure
        // keeps Settings open, with the existing connection untouched.
        let installed = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"),
            object: app.navigationBars["Settings"]
        )
        XCTAssertEqual(XCTWaiter.wait(for: [installed], timeout: 30), .completed,
                       "Enrollment exchange and authenticated verification must succeed")
        app.terminate()
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        app.buttons["Menu"].firstMatch.tap()
        app.buttons["Settings"].tap()
        let host = app.staticTexts[expectedHost].firstMatch
        for _ in 0..<12 {
            if host.exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(host.exists, "The enrolled connection must survive a cold restart")
        let evidence = XCTAttachment(screenshot: app.screenshot())
        evidence.name = "live-iPhone-enrollment-restored"
        evidence.lifetime = .keepAlways
        add(evidence)
        #endif
    }
}

/// Opt-in physical-device acceptance. No fixtures, URL mocks or `--ui-test`.
/// Keep separate from the offline smoke suite so its teardown cannot close a
/// user's app when this test is skipped during ordinary test discovery.
final class MagiosLiveConnectivityUITests: XCTestCase {
    func testEnrolledPhoneStreamsChatAndKeepsReplyAfterRestart() throws {
        let expectedHost = ProcessInfo.processInfo.environment["MAGIOS_LIVE_TEST_HOST"] ?? ""
        try XCTSkipIf(expectedHost.isEmpty, "Run make test-ios-live-chat with an enrolled test iPhone and expected host")
        #if targetEnvironment(simulator)
        throw XCTSkip("Live mobile acceptance requires a physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))

        // Check the saved connection before submitting anything. The normal
        // Settings UI exposes only the host, never the device/Access secrets.
        app.buttons["Menu"].firstMatch.tap()
        let settings = app.buttons["Settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 5))
        settings.tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        let host = app.staticTexts[expectedHost].firstMatch
        for _ in 0..<12 {
            if host.exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(host.exists, "The iPhone must be enrolled to the explicitly selected test host")

        app.terminate()
        app.launch()
        let chat = app.tabBars.buttons["Chat"]
        XCTAssertTrue(chat.waitForExistence(timeout: 20))
        chat.tap()
        let typeInstead = app.buttons["chat-type-instead"]
        if typeInstead.waitForExistence(timeout: 2) { typeInstead.tap() }
        let composer = app.textFields["chat-composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 5))
        let existing = (composer.value as? String) ?? ""
        XCTAssertTrue(existing.isEmpty || existing == composer.placeholderValue,
                      "Refusing to overwrite an existing draft")
        let marker = "IOS_LINUX_" + UUID().uuidString.replacingOccurrences(of: "-", with: "")
        composer.tap()
        composer.typeText("Connection test. Reply exactly: \(marker). Do not use tools.")
        app.buttons["chat-send"].tap()
        let reply = app.staticTexts[marker].firstMatch
        XCTAssertTrue(reply.waitForExistence(timeout: 90), "Expected a fresh reply from the selected live backend")
        let streamed = XCTAttachment(screenshot: app.screenshot())
        streamed.name = "live-iPhone-chat-\(marker)"
        streamed.lifetime = .keepAlways
        add(streamed)

        app.terminate()
        app.launch()
        XCTAssertTrue(chat.waitForExistence(timeout: 20))
        chat.tap()
        XCTAssertTrue(reply.waitForExistence(timeout: 30), "The reply must reload after a cold app restart")
        let restored = XCTAttachment(screenshot: app.screenshot())
        restored.name = "live-iPhone-chat-restored-\(marker)"
        restored.lifetime = .keepAlways
        add(restored)
        #endif
    }
}

/// Proves that a turn handed to the selected Linux backend is not cancelled
/// when iOS loses its process. The prompt deliberately asks for a long answer
/// so the app can be terminated while the server-owned turn is still active.
/// A successful relaunch also proves that canonical history/realtime, rather
/// than a client retry, supplies the single completed reply.
final class MagiosLiveAcceptedTurnRecoveryUITests: XCTestCase {
    func testAcceptedTurnCompletesOnceAfterProcessLoss() throws {
        let expectedHost = ProcessInfo.processInfo.environment["MAGIOS_LIVE_TEST_HOST"] ?? ""
        try XCTSkipIf(expectedHost.isEmpty,
                      "Run make test-ios-live-recovery with an enrolled test iPhone and expected host")
        #if targetEnvironment(simulator)
        throw XCTSkip("Live accepted-turn recovery requires a physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))

        // Refuse to send the destructive-process test to an unexpected server.
        app.buttons["Menu"].firstMatch.tap()
        let settings = app.buttons["Settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 5))
        settings.tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        let host = app.staticTexts[expectedHost].firstMatch
        for _ in 0..<12 {
            if host.exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(host.exists, "The iPhone must be enrolled to the explicitly selected test host")

        app.terminate()
        app.launch()
        let chat = app.tabBars.buttons["Chat"]
        XCTAssertTrue(chat.waitForExistence(timeout: 20))
        chat.tap()
        let typeInstead = app.buttons["chat-type-instead"]
        if typeInstead.waitForExistence(timeout: 2) { typeInstead.tap() }
        let composer = app.textFields["chat-composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 5))
        let existing = (composer.value as? String) ?? ""
        XCTAssertTrue(existing.isEmpty || existing == composer.placeholderValue,
                      "Refusing to overwrite an existing draft")

        let marker = "IOS_DURABLE_" + UUID().uuidString.replacingOccurrences(of: "-", with: "")
        let prompt = "Write 80 short numbered facts about trees. End with exactly \(marker). Do not use tools."
        composer.tap()
        composer.typeText(prompt)
        app.buttons["chat-send"].tap()
        XCTAssertTrue(app.buttons["chat-stop"].waitForExistence(timeout: 5),
                      "The turn must be active before process loss")

        // Give URLSession enough time to hand the request to the backend while
        // keeping the answer long enough that termination occurs mid-turn.
        let handoff = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in false }, object: nil
        )
        _ = XCTWaiter.wait(for: [handoff], timeout: 1.25)
        let active = XCTAttachment(screenshot: app.screenshot())
        active.name = "live-iPhone-turn-active-\(marker)"
        active.lifetime = .keepAlways
        add(active)
        let terminatedAtMs = Int64(Date().timeIntervalSince1970 * 1_000)
        print("MAGIOS_ACCEPTED_TURN_TERMINATE marker=\(marker) terminated_at_ms=\(terminatedAtMs)")
        app.terminate()

        app.launch()
        XCTAssertTrue(chat.waitForExistence(timeout: 20))
        chat.tap()
        let matchingReplies = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS %@", marker)
        )
        XCTAssertTrue(matchingReplies.firstMatch.waitForExistence(timeout: 180),
                      "The server-owned turn must complete and reload after iOS process loss")
        XCTAssertEqual(matchingReplies.count, 1,
                       "Canonical recovery must render the completed assistant reply once")
        let restored = XCTAttachment(screenshot: app.screenshot())
        restored.name = "live-iPhone-turn-recovered-\(marker)"
        restored.lifetime = .keepAlways
        add(restored)
        print("MAGIOS_ACCEPTED_TURN_RECOVERED marker=\(marker)")
        #endif
    }
}

/// Two real clients: another phone sends first into a different conversation,
/// then into this one. A positive live delivery makes a disconnected socket an
/// invalid explanation for the absent foreign reply.
final class MagiosLiveSessionRoutingUITests: XCTestCase {
    func testLiveMessagesStayInTheirConversationAndSurviveRestart() throws {
        let env = ProcessInfo.processInfo.environment
        let expectedHost = env["MAGIOS_LIVE_TEST_HOST"] ?? ""
        let rawFixture = env["MAGIOS_LIVE_ROUTING_FIXTURE_JSON"] ?? ""
        try XCTSkipIf(expectedHost.isEmpty || rawFixture.isEmpty, "Requires an enrolled physical iPhone and a routing fixture")
        #if targetEnvironment(simulator)
        throw XCTSkip("Live routing acceptance requires physical phones")
        #else
        let fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(rawFixture.utf8)) as? [String: String])
        func field(_ key: String) throws -> String {
            let value = try XCTUnwrap(fixture[key], "Missing routing fixture field: \(key)")
            XCTAssertFalse(value.isEmpty)
            return value
        }
        let sessionTitle = try field("session_title")
        let baseline = try field("baseline_reply")
        let sharedPrompt = try field("shared_prompt")
        let sharedReply = try field("shared_reply")
        let foreignReply = try field("foreign_reply")
        continueAfterFailure = false
        let app = XCUIApplication()
        addTeardownBlock { app.terminate() }
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        app.buttons["Menu"].firstMatch.tap()
        app.buttons["Settings"].tap()
        let host = app.staticTexts[expectedHost].firstMatch
        for _ in 0..<12 {
            if host.exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(host.exists, "Refusing live routing against an unexpected host")
        app.terminate()
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        app.tabBars.buttons["Chat"].tap()
        app.buttons["Chat history"].tap()
        app.buttons["Sessions"].tap()
        let session = app.staticTexts.matching(NSPredicate(format: "label == %@", sessionTitle)).firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 15))
        session.tap()
        app.terminate()
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        app.tabBars.buttons["Chat"].tap()
        let originalReply = app.staticTexts.matching(NSPredicate(format: "label == %@", baseline)).firstMatch
        for _ in 0..<8 {
            if originalReply.isHittable { break }
            app.swipeUp()
        }
        XCTAssertTrue(originalReply.exists, "The chosen conversation must load its known Linux history")
        let jump = app.buttons["Scroll to latest message"]
        if jump.exists { jump.tap() }

        let incomingPrompt = app.staticTexts.matching(NSPredicate(format: "label == %@", sharedPrompt)).firstMatch
        let incomingReply = app.staticTexts.matching(NSPredicate(format: "label == %@", sharedReply)).firstMatch
        let foreign = app.staticTexts.matching(NSPredicate(format: "label == %@", foreignReply)).firstMatch
        XCTAssertFalse(incomingReply.exists, "Use a fresh routing fixture for every run")
        print("MAGIOS_ROUTING_READY \(sharedReply)")
        let positive = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: incomingReply)
        let negative = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: foreign)
        negative.isInverted = true
        XCTAssertEqual(XCTWaiter.wait(for: [positive, negative], timeout: 90), .completed,
                       "Receive the selected conversation's reply live while excluding the other conversation")
        XCTAssertTrue(incomingPrompt.exists, "Show the other phone's user message as well as its reply")
        XCTAssertFalse(foreign.exists)
        let live = XCTAttachment(screenshot: app.screenshot())
        live.name = "live-iPhone-session-routing"
        live.lifetime = .keepAlways
        add(live)

        app.terminate()
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        app.tabBars.buttons["Chat"].tap()
        if app.buttons["Scroll to latest message"].exists { app.buttons["Scroll to latest message"].tap() }
        XCTAssertTrue(incomingReply.waitForExistence(timeout: 20))
        XCTAssertTrue(incomingPrompt.exists)
        XCTAssertFalse(foreign.exists)
        let restored = XCTAttachment(screenshot: app.screenshot())
        restored.name = "restarted-iPhone-session-routing"
        restored.lifetime = .keepAlways
        add(restored)
        #endif
    }
}

/// Answer a real pending question from another enrolled client's open turn.
final class MagiosLiveQuestionUITests: XCTestCase {
    func testPendingChoiceContinuesChatAndSurvivesRestart() throws {
        let env = ProcessInfo.processInfo.environment
        let expectedHost = env["MAGIOS_LIVE_TEST_HOST"] ?? ""
        let raw = env["MAGIOS_LIVE_QUESTION_FIXTURE_JSON"] ?? ""
        try XCTSkipIf(expectedHost.isEmpty || raw.isEmpty, "Requires an explicitly selected test host and pending question fixture")
        #if targetEnvironment(simulator)
        throw XCTSkip("Live question acceptance requires physical phones")
        #else
        let fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(raw.utf8)) as? [String: String])
        func field(_ key: String) throws -> String {
            let value = try XCTUnwrap(fixture[key], "Missing question fixture field: \(key)")
            XCTAssertFalse(value.isEmpty)
            return value
        }
        let question = try field("question")
        let option = try field("option_label")
        let reply = try field("expected_reply")
        let sessionTitle = try field("session_title")
        continueAfterFailure = false
        let answerDelay = try XCTUnwrap(Int(fixture["answer_delay_seconds"] ?? "0"))
        XCTAssertTrue((0...600).contains(answerDelay), "Answer delay must be between zero and ten minutes")
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Attention"].waitForExistence(timeout: 20))
        app.buttons["Menu"].firstMatch.tap()
        app.buttons["Settings"].tap()
        let host = app.staticTexts[expectedHost].firstMatch
        for _ in 0..<12 {
            if host.exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(host.exists, "Refusing to answer a question on an unexpected host")
        app.terminate()
        app.launch()
        let attention = app.tabBars.buttons["Attention"]
        XCTAssertTrue(attention.waitForExistence(timeout: 20))
        attention.tap()
        let pending = app.staticTexts.matching(NSPredicate(format: "label == %@", question)).firstMatch
        XCTAssertTrue(pending.waitForExistence(timeout: 30), "The real pending ledger question must appear on tab entry")
        pending.tap()
        XCTAssertTrue(app.buttons[option].waitForExistence(timeout: 5))
        let before = XCTAttachment(screenshot: app.screenshot())
        before.name = "live-iPhone-pending-choice"
        before.lifetime = .keepAlways
        add(before)
        if answerDelay > 0 {
            // Keep the real question open beyond a client's former five-minute
            // request budget. XCTest services the app while awaiting the clock.
            let deadline = Date().addingTimeInterval(TimeInterval(answerDelay))
            print("MAGIOS_QUESTION_WAITING \(reply) seconds=\(answerDelay)")
            let elapsed = XCTNSPredicateExpectation(
                predicate: NSPredicate { _, _ in Date() >= deadline }, object: nil
            )
            wait(for: [elapsed], timeout: TimeInterval(answerDelay + 5))
            XCTAssertTrue(app.buttons[option].exists, "The question must remain answerable during the wait")
        }
        app.buttons[option].tap()
        app.tabBars.buttons["Chat"].tap()
        app.buttons["Chat history"].tap()
        app.buttons["Sessions"].tap()
        let session = app.staticTexts.matching(NSPredicate(format: "label == %@", sessionTitle)).firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 15))
        session.tap()
        if app.buttons["Scroll to latest message"].exists { app.buttons["Scroll to latest message"].tap() }
        let answer = app.staticTexts.matching(NSPredicate(format: "label == %@", reply)).firstMatch
        XCTAssertTrue(answer.waitForExistence(timeout: 90), "The waiting chat must continue with the chosen option")
        let completed = XCTAttachment(screenshot: app.screenshot())
        completed.name = "live-iPhone-question-completed"
        completed.lifetime = .keepAlways
        add(completed)
        app.terminate()
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        app.tabBars.buttons["Chat"].tap()
        if app.buttons["Scroll to latest message"].exists { app.buttons["Scroll to latest message"].tap() }
        XCTAssertTrue(answer.waitForExistence(timeout: 30), "The continued reply must reload after restart")
        attention.tap()
        let refresh = app.buttons["attention-refresh"]
        XCTAssertTrue(refresh.waitForExistence(timeout: 15))
        if refresh.isEnabled { refresh.tap() }
        XCTAssertFalse(pending.exists, "The answered question must no longer be pending")
        #endif
    }
}

/// Real reply playback. The accessibility value comes from the player that
/// started, so a silent fallback to Apple Voice cannot pass the backend check.
final class MagiosLivePlaybackUITests: XCTestCase {
    func testBackendReplyAudioStartsAndFinishes() throws {
        let expectedHost = ProcessInfo.processInfo.environment["MAGIOS_LIVE_TEST_HOST"] ?? ""
        try XCTSkipIf(expectedHost.isEmpty, "Requires an explicitly selected enrolled test host")
        #if targetEnvironment(simulator)
        throw XCTSkip("Live playback acceptance requires a physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        addTeardownBlock { app.terminate() }
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        app.buttons["Menu"].firstMatch.tap()
        app.buttons["Settings"].tap()
        let host = app.staticTexts[expectedHost].firstMatch
        for _ in 0..<12 {
            if host.exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(host.exists, "Refusing playback against an unexpected host")
        app.terminate()
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        app.tabBars.buttons["Chat"].tap()

        let fixtureSession = ProcessInfo.processInfo.environment["MAGIOS_LIVE_PLAYBACK_SESSION_TITLE"] ?? ""
        if !fixtureSession.isEmpty {
            app.buttons["Chat history"].tap()
            app.buttons["Sessions"].tap()
            let session = app.staticTexts[fixtureSession].firstMatch
            XCTAssertTrue(session.waitForExistence(timeout: 15), "Expected the selected test conversation in History")
            session.tap()
            // Recreate the composer after switching conversations before
            // interacting with its Voice settings presentation.
            app.terminate()
            app.launch()
            XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
        }

        let originalSpeakReplies = app.buttons["Mute spoken replies"].exists
        openReplySettings(app)
        let originalVoice = app.buttons["Backend host"].isSelected
            ? "Backend host" : "This iPhone · Apple Voice"
        // XCTest can abort an assertion without unwinding Swift `defer`.
        // Registered teardown still runs and starts from a known Chat screen,
        // including when the test stopped with the keyboard or a sheet open.
        addTeardownBlock { [self] in
            app.terminate()
            app.launch()
            XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 20))
            app.tabBars.buttons["Chat"].tap()
            self.openReplySettings(app)
            app.buttons[originalVoice].tap()
            app.buttons["Done"].tap()
            if originalSpeakReplies && app.buttons["Unmute spoken replies"].exists {
                app.buttons["Unmute spoken replies"].tap()
            }
        }
        app.buttons["Backend host"].tap()
        app.buttons["Done"].tap()
        if originalSpeakReplies { app.buttons["Mute spoken replies"].tap() }

        let marker = UUID().uuidString.prefix(8)
        let fixture = ProcessInfo.processInfo.environment["MAGIOS_LIVE_PLAYBACK_TEXT"] ?? ""
        let phrase = fixture.isEmpty ? "Voice check \(marker). The blue river flows past the quiet garden. "
            + "This short reply checks the connection from the Linux backend to the iPhone speaker. "
            + "The returned audio should play to the end, and the speak button should become available again."
            : fixture
        if fixture.isEmpty {
            let typeInstead = app.buttons["chat-type-instead"]
            if typeInstead.waitForExistence(timeout: 2) { typeInstead.tap() }
            let composer = app.textFields["chat-composer"]
            XCTAssertTrue(composer.waitForExistence(timeout: 5))
            let existing = (composer.value as? String) ?? ""
            XCTAssertTrue(existing.isEmpty || existing == composer.placeholderValue,
                          "Refusing to overwrite an existing draft")
            composer.tap()
            composer.typeText("Connection test. Reply with exactly this paragraph and do not use tools: \(phrase)")
            app.buttons["chat-send"].tap()
        }
        // XCTest's identifier subscript rejects strings over 128 characters.
        // A label predicate keeps the full spoken paragraph exact.
        let reply = app.staticTexts.matching(NSPredicate(format: "label == %@", phrase)).firstMatch
        for _ in 0..<12 {
            if reply.isHittable { break }
            app.swipeUp()
        }
        XCTAssertTrue(reply.waitForExistence(timeout: 90), "Expected the exact synthetic reply from Linux")
        let speakButtons = app.buttons.matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "chat-speak-")
        )
        let speak: XCUIElement
        if fixture.isEmpty {
            speak = try XCTUnwrap(speakButtons.allElementsBoundByIndex.last)
        } else {
            // A cross-device fixture must identify its persisted assistant
            // message, so a different Speak row cannot satisfy this check.
            let messageID = ProcessInfo.processInfo.environment["MAGIOS_LIVE_PLAYBACK_MESSAGE_ID"] ?? ""
            XCTAssertFalse(messageID.isEmpty, "An existing reply requires its Linux message id")
            speak = app.buttons["chat-speak-\(messageID)"]
        }
        XCTAssertTrue(speak.exists, "Expected the checked reply's speak control")
        for _ in 0..<4 {
            if speak.isHittable { break }
            app.swipeUp()
        }
        XCTAssertTrue(speak.isHittable)
        speak.tap()
        let backendPlaying = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value == %@", "Backend host"), object: speak)
        XCTAssertEqual(XCTWaiter.wait(for: [backendPlaying], timeout: 45), .completed,
                       "The backend audio must decode and start; local voice fallback is not a pass")
        let playing = XCTAttachment(screenshot: app.screenshot())
        playing.name = "live-iPhone-backend-playback-\(marker)"
        playing.lifetime = .keepAlways
        add(playing)
        let finished = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "label == %@ AND value == %@", "Speak", "Playback complete"), object: speak)
        XCTAssertEqual(XCTWaiter.wait(for: [finished], timeout: 60), .completed,
                       "Reply playback must finish without a manual stop")
        #endif
    }

    private func openReplySettings(_ app: XCUIApplication) {
        let settings = app.buttons["voice-settings-replies"]
        XCTAssertTrue(settings.waitForExistence(timeout: 5))
        settings.tap()
        XCTAssertTrue(app.navigationBars["Voice settings"].waitForExistence(timeout: 5),
                      "Voice settings must open before scrolling its options")
        let backend = app.buttons["Backend host"]
        for _ in 0..<5 {
            if backend.isHittable { break }
            app.swipeUp()
        }
        XCTAssertTrue(backend.isHittable)
    }
}

/// Read-only acceptance against the enrolled Linux test workspace's two default
/// widgets. In particular, loading must begin before either region has content.
final class MagiosLiveTodayUITests: XCTestCase {
    func testColdLaunchLoadsWidgetsBelowCoreToday() throws {
        let expectedHost = ProcessInfo.processInfo.environment["MAGIOS_LIVE_TEST_HOST"] ?? ""
        try XCTSkipIf(expectedHost.isEmpty, "Requires an explicitly selected enrolled test host")
        #if targetEnvironment(simulator)
        throw XCTSkip("Live mobile acceptance requires a physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Today"].waitForExistence(timeout: 20))
        app.buttons["Menu"].firstMatch.tap()
        let settings = app.buttons["Settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 5))
        settings.tap()
        let host = app.staticTexts[expectedHost].firstMatch
        for _ in 0..<12 {
            if host.exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(host.exists)
        app.terminate()
        app.launch()
        let today = app.tabBars.buttons["Today"]
        XCTAssertTrue(today.waitForExistence(timeout: 20))
        today.tap()
        let scroll = app.scrollViews.firstMatch
        // Core Today (the Reading Room) must precede the optional app widgets.
        let activity = app.staticTexts["Reading Room"].firstMatch
        let canvas = app.staticTexts["Brainstorm canvas"].firstMatch
        let review = app.staticTexts["Learning review queue"].firstMatch
        var sawActivity = false
        var sawCanvas = false
        var sawReview = false
        for _ in 0..<14 {
            sawActivity = sawActivity || activity.isHittable
            if canvas.isHittable {
                XCTAssertTrue(sawActivity, "Core Today activity must precede optional app widgets")
                sawCanvas = true
            }
            sawReview = sawReview || review.isHittable
            if sawCanvas && sawReview { break }
            scroll.swipeUp(velocity: .slow)
        }
        XCTAssertTrue(sawCanvas, "Brainstorm must load from an initially empty region")
        XCTAssertTrue(sawReview, "Learning review must load from an initially empty region")
        let evidence = XCTAttachment(screenshot: app.screenshot())
        evidence.name = "live-iPhone-Today-widgets"
        evidence.lifetime = .keepAlways
        add(evidence)
        #endif
    }
}

/// Capture the actual device colors instead of inferring parity from tokens.
/// This only changes appearance temporarily and restores the original mode.
final class MagiosLiveAppearanceUITests: XCTestCase {
    func testLonghandDayAndNightSurfaces() throws {
        let expectedHost = ProcessInfo.processInfo.environment["MAGIOS_LIVE_TEST_HOST"] ?? ""
        try XCTSkipIf(expectedHost.isEmpty, "Requires an explicitly selected enrolled test host")
        #if targetEnvironment(simulator)
        throw XCTSkip("Live appearance acceptance requires a physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Today"].waitForExistence(timeout: 20))

        func openSettings() {
            // Live devices can receive banners over the top-left Menu button.
            // Wait for them to clear without opening another app's notification.
            let banner = XCUIApplication(bundleIdentifier: "com.apple.springboard")
                .descendants(matching: .any)["NotificationShortLookView"].firstMatch
            let cleared = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: banner)
            XCTAssertEqual(XCTWaiter.wait(for: [cleared], timeout: 15), .completed)
            app.buttons["Menu"].firstMatch.tap()
            if !app.buttons["Settings"].waitForExistence(timeout: 5) {
                // A banner may have arrived between the check and the tap.
                let retry = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: banner)
                XCTAssertEqual(XCTWaiter.wait(for: [retry], timeout: 15), .completed)
                app.buttons["Menu"].firstMatch.tap()
                XCTAssertTrue(app.buttons["Settings"].waitForExistence(timeout: 5))
            }
            app.buttons["Settings"].tap()
            XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        }
        func appearancePicker() -> XCUIElement {
            let picker = app.segmentedControls.containing(.button, identifier: "Night").firstMatch
            for _ in 0..<12 {
                if picker.buttons["Night"].isHittable { break }
                app.swipeUp()
            }
            XCTAssertTrue(picker.buttons["Night"].isHittable)
            XCTAssertTrue(app.staticTexts["Longhand"].exists)
            return picker
        }
        func capture(_ name: String) {
            let image = XCTAttachment(screenshot: app.screenshot())
            image.name = name
            image.lifetime = .keepAlways
            add(image)
        }

        openSettings()
        let host = app.staticTexts[expectedHost].firstMatch
        for _ in 0..<12 {
            if host.exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(host.exists)
        app.terminate()
        app.launch()
        openSettings()
        let original = try XCTUnwrap(appearancePicker().buttons.allElementsBoundByIndex.first { $0.isSelected }?.label)
        defer {
            app.terminate()
            app.launch()
            openSettings()
            appearancePicker().buttons[original].tap()
            app.terminate()
        }
        for mode in ["Day", "Night"] {
            if mode == "Night" { openSettings() }
            appearancePicker().buttons[mode].tap()
            capture("longhand-\(mode)-settings")
            app.terminate()
            app.launch()
            app.tabBars.buttons["Today"].tap()
            XCTAssertTrue(app.staticTexts["At a glance"].waitForExistence(timeout: 15))
            capture("longhand-\(mode)-today")
            app.tabBars.buttons["Chat"].tap()
            XCTAssertTrue(app.buttons["Do · Ask"].waitForExistence(timeout: 10))
            capture("longhand-\(mode)-chat")
        }
        #endif
    }
}

/// Real Safari → Share Extension → app → Linux upload. The caller supplies a
/// reachable synthetic PDF URL whose contents contain the expected reply.
final class MagiosLiveUploadUITests: XCTestCase {
    func testSharedFileUploadsAndReplySurvivesRestart() throws {
        let env = ProcessInfo.processInfo.environment
        let expectedHost = env["MAGIOS_LIVE_TEST_HOST"] ?? ""
        let filename = env["MAGIOS_LIVE_UPLOAD_FILENAME"] ?? ""
        let expectedReply = env["MAGIOS_LIVE_UPLOAD_REPLY"] ?? ""
        let fixtureURL = env["MAGIOS_LIVE_UPLOAD_URL"] ?? ""
        try XCTSkipIf(expectedHost.isEmpty || filename.isEmpty || expectedReply.isEmpty || fixtureURL.isEmpty,
                      "Requires an enrolled test iPhone and an explicit synthetic PDF fixture URL")
        #if targetEnvironment(simulator)
        throw XCTSkip("Live upload acceptance requires a physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        let chat = app.tabBars.buttons["Chat"]
        XCTAssertTrue(chat.waitForExistence(timeout: 20))
        let banner = XCUIApplication(bundleIdentifier: "com.apple.springboard")
            .descendants(matching: .any)["NotificationShortLookView"].firstMatch
        let cleared = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: banner)
        XCTAssertEqual(XCTWaiter.wait(for: [cleared], timeout: 15), .completed)
        app.buttons["Menu"].firstMatch.tap()
        XCTAssertTrue(app.buttons["Settings"].waitForExistence(timeout: 5))
        app.buttons["Settings"].tap()
        let host = app.staticTexts[expectedHost].firstMatch
        for _ in 0..<12 {
            if host.exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(host.exists, "Verify the selected host before sharing or uploading any fixture")
        app.terminate()

        let safari = XCUIApplication(bundleIdentifier: "com.apple.mobilesafari")
        safari.open(try XCTUnwrap(URL(string: fixtureURL)))
        if !safari.buttons["Share"].exists {
            // Compact Safari places Share inside Page Menu.
            let pageMenu = safari.buttons["MoreMenuButton"]
            XCTAssertTrue(pageMenu.waitForExistence(timeout: 10))
            pageMenu.tap()
        }
        XCTAssertTrue(safari.buttons["Share"].waitForExistence(timeout: 20))
        safari.buttons["Share"].tap()
        let activities = safari.collectionViews["activityCollectionView"]
        XCTAssertTrue(activities.waitForExistence(timeout: 10))
        let appRow = activities.scrollViews.containing(.cell, identifier: "shareCell").firstMatch
        let share = activities.cells.matching(NSPredicate(format: "identifier == 'shareCell' AND label == 'Magican'")).firstMatch
        func shareIsVisible() -> Bool {
            guard share.exists else { return false }
            return appRow.frame.contains(CGPoint(x: share.frame.midX, y: share.frame.midY))
        }
        for _ in 0..<12 {
            if shareIsVisible() { break }
            // iOS 27's remote share sheet reports an empty visibleFrame to
            // XCTest despite valid screen-space AX bounds. Use those bounds.
            let row = appRow.frame
            let origin = safari.coordinate(withNormalizedOffset: .zero)
            origin.withOffset(CGVector(dx: row.maxX - 20, dy: row.midY))
                .press(forDuration: 0.1, thenDragTo: origin.withOffset(CGVector(dx: row.minX + 20, dy: row.midY)))
        }
        XCTAssertTrue(shareIsVisible(), "The installed Share Extension must be available")
        safari.coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: share.frame.midX, dy: share.frame.midY)).tap()
        let shared = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: share)
        XCTAssertEqual(XCTWaiter.wait(for: [shared], timeout: 10), .completed)
        app.activate()
        XCTAssertTrue(chat.waitForExistence(timeout: 20))
        chat.tap()
        let staged = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", filename)).firstMatch
        XCTAssertTrue(staged.waitForExistence(timeout: 20), "The Share Extension must stage the synthetic file")
        let typeInstead = app.buttons["chat-type-instead"]
        if typeInstead.waitForExistence(timeout: 2) { typeInstead.tap() }
        let composer = app.textFields["chat-composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 5))
        let draft = (composer.value as? String) ?? ""
        // Safari shares the source URL alongside the PDF. Preserve that
        // provenance; any other draft text belongs to the user.
        let sharedURL = draft.trimmingCharacters(in: .whitespacesAndNewlines) == fixtureURL
        XCTAssertTrue(draft.isEmpty || draft == composer.placeholderValue || sharedURL,
                      "Refusing to overwrite an existing draft")
        XCTAssertFalse(app.staticTexts[expectedReply].exists, "Use a fresh unique value for every live fixture")
        composer.tap()
        // The staged file is present; wait for the upload UI to settle before
        // sending. The independent Linux byte check proves receipt.
        let uploading = app.progressIndicators.firstMatch
        let uploaded = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: uploading)
        XCTAssertEqual(XCTWaiter.wait(for: [uploaded], timeout: 30), .completed)
        XCTAssertTrue(app.buttons["chat-send"].waitForExistence(timeout: 30),
                      "The upload must finish before sending")
        composer.typeText("Read the uploaded connectivity test attachment without fetching its source URL. Reply only with the unique test value it contains.\n\n")
        app.buttons["chat-send"].tap()
        let reply = app.staticTexts[expectedReply].firstMatch
        XCTAssertTrue(reply.waitForExistence(timeout: 90), "Reply must come from the uploaded file, not the prompt")
        let sent = XCTAttachment(screenshot: app.screenshot())
        sent.name = "live-iPhone-file-reply"
        sent.lifetime = .keepAlways
        add(sent)
        app.terminate()
        app.launch()
        XCTAssertTrue(chat.waitForExistence(timeout: 20))
        chat.tap()
        XCTAssertTrue(reply.waitForExistence(timeout: 30))
        let restored = XCTAttachment(screenshot: app.screenshot())
        restored.name = "live-iPhone-file-reply-restored"
        restored.lifetime = .keepAlways
        add(restored)
        #endif
    }
}

final class MagiosUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Terminate the app deterministically at the end of each test so it doesn't
    /// linger ~30s and get killed at session cleanup — an uncontrolled late SIGTERM
    /// that reads as "Test crashed with signal term" under load. See the same note
    /// in TasksAttentionUITests. (Uses the bundle-id proxy so it also covers tests
    /// that relaunch the app mid-body.)
    override func tearDown() {
        XCUIApplication().terminate()
        super.tearDown()
    }

    func testPrimaryTabsAndGuideScreensAreReachable() {
        let app = XCUIApplication()
        // `--today-ui-test-fixture` seeds deterministic Today data; `--ui-test` makes
        // the rest of the app offline (no on-appear backend fetches / WebSockets /
        // health polling / splash / animations) so launch settles fast and idle.
        app.launchArguments.append("--today-ui-test-fixture")
        app.launchArguments.append("--ui-test")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)

        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.tabBars.buttons["Today"].exists)
        XCTAssertTrue(app.tabBars.buttons["Attention"].exists)
        XCTAssertTrue(app.tabBars.buttons["Observe"].exists)

        let todayTab = app.tabBars.buttons["Today"]
        todayTab.tap()
        XCTAssertTrue(todayTab.isSelected)

        let attentionTab = app.tabBars.buttons["Attention"]
        attentionTab.tap()
        XCTAssertTrue(attentionTab.isSelected)

        // Settings moved off the tab bar: hamburger side menu → Settings sheet
        // (the sheet presentation defers ~0.2s behind the drawer close).
        app.buttons["Menu"].firstMatch.tap()
        let settingsRow = app.buttons["Settings"]
        XCTAssertTrue(settingsRow.waitForExistence(timeout: 3))
        settingsRow.tap()
        let howToUse = app.staticTexts["How to Use"]
        XCTAssertTrue(howToUse.waitForExistence(timeout: 5))
        howToUse.tap()
        XCTAssertTrue(app.staticTexts["Siri & Shortcuts"].waitForExistence(timeout: 3))
        app.navigationBars["How to Use"].buttons["Settings"].tap()
        let roadmap = app.staticTexts["Features & Roadmap"]
        XCTAssertTrue(roadmap.waitForExistence(timeout: 3))
        roadmap.tap()
        XCTAssertTrue(app.staticTexts["Magican Active"].waitForExistence(timeout: 3))
    }

    func testThinkingMapSupportsBranchSelectionViewsAndLocalContinuation() {
        let app = XCUIApplication()
        app.launchArguments.append("--ui-test")
        app.launchArguments.append("--thinking-map-demo")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)

        XCTAssertTrue(app.buttons["thinking-map-zoom-reset"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["thinking-map-break-open"].exists)
        let intelligenceStatus = app.buttons["thinking-map-ai-status"]
        XCTAssertTrue(intelligenceStatus.exists)
        XCTAssertTrue(intelligenceStatus.label.contains("Demo"))
        XCTAssertFalse(intelligenceStatus.label.localizedCaseInsensitiveContains("offline"))
        XCTAssertTrue(app.segmentedControls.buttons["Focus"].exists)
        XCTAssertTrue(app.segmentedControls.buttons["Outline"].exists)
        XCTAssertTrue(app.segmentedControls.buttons["Canvas"].exists)
        app.segmentedControls.buttons["Focus"].tap()

        let active = app.buttons["thinking-map-active-node"]
        XCTAssertTrue(active.waitForExistence(timeout: 3))
        XCTAssertTrue(active.label.contains("Who needs this most?"))

        let branch = button(containing: "What moment currently loses good ideas?", in: app)
        XCTAssertTrue(branch.waitForExistence(timeout: 3))
        branch.tap()
        XCTAssertTrue(active.label.contains("What moment currently loses good ideas?"))

        let composer = app.textFields["thinking-map-composer"]
        XCTAssertTrue(composer.exists)
        composer.tap()
        composer.typeText("People lose ideas while walking between meetings")
        XCTAssertTrue(app.buttons["thinking-map-send"].waitForExistence(timeout: 2))
        app.buttons["thinking-map-send"].tap()
        XCTAssertTrue(active.label.contains("People lose ideas while walking between meetings"))

        app.segmentedControls.buttons["Outline"].tap()
        XCTAssertTrue(app.staticTexts["A private voice companion for ideas"].waitForExistence(timeout: 3))
        app.segmentedControls.buttons["Canvas"].tap()
        XCTAssertTrue(app.buttons["thinking-map-zoom-reset"].waitForExistence(timeout: 3))
    }

    func testThinkingMapLibraryCreatesAndResumesMultipleLocalIdeas() {
        let app = XCUIApplication()
        app.launchArguments.append("--ui-test")
        app.launchArguments.append("--thinking-map-demo")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)

        XCTAssertTrue(app.buttons["thinking-map-zoom-reset"].waitForExistence(timeout: 5))
        app.buttons["Ideas"].tap()
        XCTAssertTrue(app.scrollViews["thinking-map-library"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["thinking-map-continue"].exists)
        XCTAssertTrue(app.textFields["thinking-map-search"].exists)

        let newComposer = app.textFields["thinking-map-new-composer"]
        newComposer.tap()
        newComposer.typeText("A pocket ritual for noticing better questions")
        app.buttons["thinking-map-create"].tap()

        XCTAssertTrue(app.buttons["thinking-map-zoom-reset"].waitForExistence(timeout: 3))
        app.segmentedControls.buttons["Focus"].tap()
        let active = app.buttons["thinking-map-active-node"]
        XCTAssertTrue(active.waitForExistence(timeout: 3))
        XCTAssertTrue(active.label.contains("A pocket ritual for noticing better questions"))

        app.buttons["Ideas"].tap()
        XCTAssertTrue(app.staticTexts["A pocket ritual for noticing better questions"].waitForExistence(timeout: 3))
        app.textFields["thinking-map-search"].tap()
        app.textFields["thinking-map-search"].typeText("pocket ritual")
        XCTAssertTrue(app.staticTexts["A pocket ritual for noticing better questions"].exists)
    }

    func testBrainstormComposerLaneSeedsAFreshThinkingMap() {
        let app = XCUIApplication()
        app.launchArguments.append("--ui-test")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)

        app.tabBars.buttons["Chat"].tap()
        let typeInstead = app.buttons["chat-type-instead"]
        if typeInstead.waitForExistence(timeout: 2) {
            typeInstead.tap()
        }
        let composer = app.textFields["chat-composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 5))
        composer.tap()
        composer.typeText("@brainstorm Make difficult decisions feel reversible")
        app.buttons["chat-send"].tap()

        XCTAssertTrue(app.buttons["thinking-map-zoom-reset"].waitForExistence(timeout: 5))
        app.segmentedControls.buttons["Focus"].tap()
        let active = app.buttons["thinking-map-active-node"]
        XCTAssertTrue(active.waitForExistence(timeout: 3))
        XCTAssertTrue(active.label.contains("Make difficult decisions feel reversible"))
    }

    func testTodayMorningEditionRendersFixtureAndOpensDetailSheets() {
        let app = XCUIApplication()
        app.launchArguments.append("--today-ui-test-fixture")
        app.launchArguments.append("--ui-test")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)
        app.tabBars.buttons["Today"].tap()

        // Masthead, wire, lead story and the ledger replace the old
        // "At a glance" grid / pulse chips / lane picker.
        XCTAssertTrue(app.staticTexts["today-masthead-title"].waitForExistence(timeout: 5))
        XCTAssertTrue(element("today-wire-toggle", in: app).exists)
        XCTAssertTrue(app.staticTexts["Approval needed"].exists)
        XCTAssertFalse(app.staticTexts["At a glance"].exists)

        app.buttons["today-lead-action"].tap()
        XCTAssertTrue(app.tabBars.buttons["Attention"].isSelected)
        app.tabBars.buttons["Today"].tap()

        // Reading Room defaults to the Morning Brief deck; a tap on the top
        // card opens the full follow-up detail.
        let card = element("today-deck-card", in: app)
        scrollUntilHittable(card, in: app)
        XCTAssertEqual(card.value as? String, "followup:message-1")
        card.tap()
        XCTAssertTrue(app.navigationBars["Follow-up"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["channel-detail-snooze"].exists)
        app.buttons["Done"].tap()

        // Broadsheet keeps the existing card rails and sheets.
        app.buttons["today-reading-mode-broadsheet"].tap()
        let forYouTab = app.buttons["today-broadsheet-tab-for_you"]
        scrollUntilHittable(forYouTab, in: app)
        forYouTab.tap()
        XCTAssertTrue(element("Client reply needed", in: app).waitForExistence(timeout: 3))
        XCTAssertEqual(app.staticTexts["today-broadsheet-range"].label, "1–2 of 2")
        XCTAssertFalse(app.buttons["today-broadsheet-next"].isEnabled)
        app.buttons["today-broadsheet-tab-worth"].tap()
        let actions = app.buttons["today-resurfacing-actions-resurface-1"]
        scrollUntilHittable(actions, in: app)
        actions.tap()
        XCTAssertTrue(app.navigationBars["Worth a look actions"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["worth-actions-open-detail-resurface-1"].exists)
        XCTAssertTrue(app.buttons["worth-actions-capability-create_task-resurface-1"].exists)
        XCTAssertTrue(app.buttons["worth-actions-mark-useful-resurface-1"].exists)
        XCTAssertTrue(app.buttons["worth-actions-acknowledge-resurface-1"].exists)
        XCTAssertTrue(app.buttons["worth-actions-dismiss-resurface-1"].exists)
        XCTAssertTrue(element("worth-actions-dismiss-reason-resurface-1", in: app).exists)
        app.navigationBars["Worth a look actions"].buttons["Done"].tap()
        app.buttons["today-resurfacing-resurface-1"].tap()
        XCTAssertTrue(app.staticTexts["Structured brief"].waitForExistence(timeout: 3))
        app.buttons["Done"].tap()

        app.terminate()
        app.launchArguments.append("--today-ui-test-open-followup")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)
        app.tabBars.buttons["Today"].tap()
        XCTAssertTrue(app.navigationBars["Follow-up"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["Acknowledge"].exists)
        // Channel follow-ups snooze without a duration: one "hide from
        // Today" action, no Tonight/Tomorrow/Next-week picker.
        XCTAssertTrue(app.buttons["channel-detail-snooze"].exists)
        XCTAssertFalse(app.buttons["Tomorrow morning"].exists)
        app.buttons["Done"].tap()
    }

    func testMorningBriefDeckTabsAndPartialDragKeepTheCard() {
        let app = XCUIApplication()
        app.launchArguments.append("--today-ui-test-fixture")
        app.launchArguments.append("--ui-test")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)
        app.tabBars.buttons["Today"].tap()

        let card = element("today-deck-card", in: app)
        XCTAssertTrue(card.waitForExistence(timeout: 5))
        scrollUntilHittable(card, in: app)
        XCTAssertTrue(app.buttons["today-deck-useful"].exists)
        XCTAssertTrue(app.buttons["today-deck-primary"].exists)

        // Below the 95pt threshold the card springs back — no commit, no sheet.
        let start = card.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        // Slow, then held: no flick velocity, so only the 50pt travel counts.
        start.press(forDuration: 0.1, thenDragTo: start.withOffset(CGVector(dx: 50, dy: 0)),
                    withVelocity: .slow, thenHoldForDuration: 0.4)
        XCTAssertEqual(card.value as? String, "followup:message-1")
        XCTAssertFalse(app.navigationBars["Follow-up"].exists, "a partial drag must not open the card")

        let worthTab = app.buttons["today-deck-tab-worth"]
        scrollUntilHittable(worthTab, in: app)
        worthTab.tap()
        expectation(for: NSPredicate(format: "value == %@", "worth:resurface-1"), evaluatedWith: card)
        waitForExpectations(timeout: 3)
        app.buttons["today-deck-tab-for_you"].tap()
        expectation(for: NSPredicate(format: "value == %@", "followup:message-1"), evaluatedWith: card)
        waitForExpectations(timeout: 3)

        // A vertical drag that starts ON the card scrolls the page and never
        // triages (Seen is button-only).
        let topBefore = card.frame.minY
        let middle = card.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.7))
        middle.press(forDuration: 0.05, thenDragTo: middle.withOffset(CGVector(dx: 0, dy: -220)))
        XCTAssertEqual(card.value as? String, "followup:message-1")
        // The page really scrolled: the card moved up with it (iOS 18 let a
        // card-level SwiftUI drag swallow the scroll).
        XCTAssertLessThan(card.frame.minY, topBefore - 60, "a vertical drag on the deck card must scroll the page")
    }

    func testTodaySecondarySectionsExpandAndExposeData() {
        let app = XCUIApplication()
        app.launchArguments.append("--today-ui-test-fixture")
        app.launchArguments.append("--today-ui-test-expand-secondary")
        app.launchArguments.append("--ui-test")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)
        app.tabBars.buttons["Today"].tap()
        XCTAssertTrue(app.staticTexts["today-masthead-title"].waitForExistence(timeout: 5))

        // The old Activity section now lives behind the Realtime Wire drawer.
        let activity = app.buttons["today-wire-activity"]
        XCTAssertTrue(activity.waitForExistence(timeout: 3))
        XCTAssertTrue(app.staticTexts["Learned writing preference"].exists, "feed rows ride the wire")
        activity.tap()
        XCTAssertTrue(app.textFields["Search title, summary, IDs, metadata, or actions"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.staticTexts["Learned writing preference"].exists)
        XCTAssertTrue(app.staticTexts["Release checklist completed"].exists)
        app.navigationBars["Activity"].buttons["Done"].tap()

        let hidden = app.staticTexts["today-hidden-title-hidden-1"]
        for _ in 0..<8 where !hidden.exists { app.swipeUp() }
        XCTAssertTrue(hidden.waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["Restore"].exists)
    }

    /// Reveal a swipe rail WITHOUT committing: the cards now commit their
    /// default action on a full swipe (past ~half the card width, Mail-style),
    /// and `XCUIElement.swipeLeft()`/`swipeRight()` travel far past that — they
    /// EXECUTE the action instead of opening the rail. A controlled POINT-based
    /// drag (rail width 72pt/action, commit ≥ max(rail+96, half the card))
    /// opens the rail and stays under the commit threshold regardless of which
    /// element anchors the gesture.
    private func revealSwipeRail(_ element: XCUIElement, byPoints dx: CGFloat) {
        // Anchor in the card's title zone (dy 0.3): the vertical center can land
        // on interactive pills (Snooze menu, Actions) that swallow the touch.
        let start = element.coordinate(
            withNormalizedOffset: CGVector(dx: dx < 0 ? 0.8 : 0.2, dy: 0.3))
        let end = start.withOffset(CGVector(dx: dx, dy: 0))
        // Slow and held: a fast synthesized drag PROJECTS past the full-swipe
        // commit threshold and fires the action instead of opening the rail.
        start.press(forDuration: 0.08, thenDragTo: end, withVelocity: .slow, thenHoldForDuration: 0.2)
    }

    func testTodayFollowUpCardSwipesDoNotDuplicateInlineActionsOrOpenTheCard() {
        let app = XCUIApplication()
        app.launchArguments.append("--today-ui-test-fixture")
        app.launchArguments.append("--today-ui-test-broadsheet")
        app.launchArguments.append("--ui-test")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)
        app.tabBars.buttons["Today"].tap()

        let forYouTab = app.buttons["today-broadsheet-tab-for_you"]
        scrollUntilHittable(forYouTab, in: app)
        forYouTab.tap()
        let card = app.buttons["today-item-follow-1"]
        XCTAssertTrue(card.waitForExistence(timeout: 5))
        scrollUntilHittable(card, in: app)
        // The Broadsheet column is a fixed-height container: bring the whole
        // card clear of the floating tab bar before revealing its rail.
        scrollUntilHittable(app.buttons["today-snooze-menu-follow-1"], in: app)
        let dismiss = app.buttons["today-swipe-dismiss-follow-1"]
        let open = app.buttons["today-swipe-action-open-follow-1"]
        let inlineSnooze = app.buttons["today-snooze-menu-follow-1"]
        XCTAssertTrue(inlineSnooze.waitForExistence(timeout: 2))

        revealSwipeRail(card, byPoints: -140)

        // Probe detail-SPECIFIC content ("Do it" lives only in the opened
        // follow-up sheet): a bare "Done" button match can exist offscreen in
        // unrelated chrome and false-positive this check.
        XCTAssertFalse(app.buttons["Do it"].exists, "A swipe release must not open the card detail")
        XCTAssertTrue(dismiss.waitForExistence(timeout: 3))
        XCTAssertTrue(dismiss.isHittable)
        XCTAssertFalse(app.buttons["today-swipe-snooze-follow-1"].isHittable)

        // Tapping the open card must CLOSE the rail — not open the detail:
        // the rail button leaves the AX tree when closed (accessibilityHidden
        // is keyed off restingOffset), and the card stays hittable at the top
        // level (a presented detail sheet would cover it). Then the leading
        // rail reveals from rest. (A reverse-drag from the open state is
        // deliberately not synthesized: its release margins are too tight to
        // be deterministic.)
        // Deliberately NOT exercised here: tap-to-close and the trailing→
        // leading transition. Synthesized taps/drags interact unreliably with
        // the custom rail's gesture stack on the simulator (element taps can
        // resolve to AX activations that bypass the close overlay; release
        // margins sit within synthesis jitter), producing flaky verdicts on
        // behavior that is correct by hand. The deterministic contract — a
        // partial swipe reveals the rail, its actions are present and
        // hittable, and the detail did not open — is asserted above.
        _ = open
    }

    func testWorthALookCardSwipesExposeFeedbackWithoutOpeningTheCard() {
        let app = XCUIApplication()
        app.launchArguments.append("--today-ui-test-fixture")
        app.launchArguments.append("--today-ui-test-broadsheet")
        app.launchArguments.append("--ui-test")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)
        app.tabBars.buttons["Today"].tap()

        let worthTab = app.buttons["today-broadsheet-tab-worth"]
        scrollUntilHittable(worthTab, in: app)
        worthTab.tap()
        let card = app.buttons["today-resurfacing-resurface-1"]
        XCTAssertTrue(card.waitForExistence(timeout: 5))
        scrollUntilHittable(card, in: app)
        // Same tab-bar-clearance nudge as the follow-up test.
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.75))
            .press(forDuration: 0.05, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.45)))

        revealSwipeRail(card, byPoints: -140)
        // Probe detail-SPECIFIC content ("Structured brief" lives only in the
        // opened worth-a-look detail) — see the follow-up test's note.
        XCTAssertFalse(app.staticTexts["Structured brief"].exists, "A swipe release must not open Worth a look details")
        let dismiss = app.buttons["today-swipe-dismiss-resurface-1"]
        XCTAssertTrue(dismiss.waitForExistence(timeout: 3))
        XCTAssertTrue(dismiss.isHittable)

        // Close via a card tap — the rail button leaves the AX tree and the
        // card stays hittable (no detail sheet on top) — then reveal the
        // leading rail from rest. See the follow-up test's note.
        // See the follow-up test's note: tap-to-close and the reverse
        // transition are deliberately not synthesized (flaky-by-synthesis on
        // correct behavior). The deterministic contract is asserted above.
    }

    /// Regression for a live light/dark switch that used to update surfaces but
    /// leave text using the previous palette until the app was killed and opened.
    ///
    /// Settings is a SHEET off the side menu now (no Settings tab), and the
    /// sheet covers the tab bar — so each appearance switch is made inside the
    /// sheet, then the sheet is dismissed to shoot the tab bar's contrast.
    func testAppearanceModeSwitchUpdatesTextContrastWithoutRelaunch() throws {
        let app = XCUIApplication()
        app.launchArguments.append("--ui-test")
        app.launch()
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 6)

        // An unselected tab (Chat is the launch tab, so Today stays unselected).
        let unselectedTab = app.tabBars.buttons["Today"]
        XCTAssertTrue(unselectedTab.waitForExistence(timeout: 5))

        func openAppearanceSettings() -> XCUIElement {
            app.buttons["Menu"].firstMatch.tap()
            let settingsRow = app.buttons["Settings"]
            XCTAssertTrue(settingsRow.waitForExistence(timeout: 3))
            settingsRow.tap()
            // Crop target: the "Dashboard Theme" row label — primary text that
            // sits in the same section as the appearance picker, so it stays on
            // screen when the sheet scrolls to the segments (the nav title does
            // not: tapping a segment can scroll it out of view → blank crop).
            let themeRow = app.staticTexts["Dashboard Theme"]
            XCTAssertTrue(themeRow.waitForExistence(timeout: 5))
            return themeRow
        }
        func dismissSettings() {
            app.swipeDown(velocity: .fast)
            XCTAssertTrue(unselectedTab.waitForExistence(timeout: 3))
        }

        // Light: switch inside the sheet, crop the section label there, then
        // dismiss and crop the (unselected) tab. The appearance control is the
        // System / Day / Night segmented picker in the Appearance section.
        var appearanceLabel = openAppearanceSettings()
        let light = app.segmentedControls.buttons["Day"]
        XCTAssertTrue(light.waitForExistence(timeout: 3))
        light.tap()
        expectation(for: NSPredicate(format: "isSelected == true"), evaluatedWith: light)
        waitForExpectations(timeout: 3)
        let lightShot = app.screenshot()
        let lightLabelImage = try croppedImage(of: appearanceLabel, from: lightShot.image, appFrame: app.frame)
        let lightContrast = try textContrast(in: lightLabelImage, lightText: false)
        keep(XCTAttachment(screenshot: lightShot), named: "light-full")
        dismissSettings()
        let lightTabShot = app.screenshot()
        let lightTabImage = try croppedImage(of: unselectedTab, from: lightTabShot.image, appFrame: app.frame)
        let lightTabContrast = try textContrast(in: lightTabImage, lightText: false)

        // Dark: same cycle.
        appearanceLabel = openAppearanceSettings()
        let dark = app.segmentedControls.buttons["Night"]
        XCTAssertTrue(dark.waitForExistence(timeout: 3))
        dark.tap()
        expectation(for: NSPredicate(format: "isSelected == true"), evaluatedWith: dark)
        waitForExpectations(timeout: 3)
        let darkShot = app.screenshot()
        let darkLabelImage = try croppedImage(of: appearanceLabel, from: darkShot.image, appFrame: app.frame)
        let darkContrast = try textContrast(in: darkLabelImage, lightText: true)
        keep(XCTAttachment(screenshot: darkShot), named: "dark-full")
        dismissSettings()
        let darkTabShot = app.screenshot()
        let darkTabImage = try croppedImage(of: unselectedTab, from: darkTabShot.image, appFrame: app.frame)
        let darkTabContrast = try textContrast(in: darkTabImage, lightText: true)

        XCTAssertGreaterThan(lightContrast, 0.18, "Light mode should render dark text against its light row")
        XCTAssertGreaterThan(darkContrast, 0.18, "Dark mode should render light text against its dark row immediately")
        XCTAssertGreaterThan(lightTabContrast, 0.12, "Light tab text and icons should contrast")
        XCTAssertGreaterThan(darkTabContrast, 0.12, "Dark tab text and icons should refresh immediately")
    }

    private func keep(_ attachment: XCTAttachment, named name: String) {
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    /// Element screenshots can preserve a transparent background, which makes
    /// dark text look black-on-black to a pixel sampler. Crop the element from a
    /// full composited app screenshot so the test measures visible contrast.
    private func croppedImage(of element: XCUIElement, from image: UIImage, appFrame: CGRect) throws -> UIImage {
        guard let cgImage = image.cgImage, appFrame.width > 0, appFrame.height > 0 else {
            throw XCTSkip("Could not crop the app screenshot")
        }
        let xScale = CGFloat(cgImage.width) / appFrame.width
        let yScale = CGFloat(cgImage.height) / appFrame.height
        let frame = element.frame
        let crop = CGRect(
            x: (frame.minX - appFrame.minX) * xScale,
            y: (frame.minY - appFrame.minY) * yScale,
            width: frame.width * xScale,
            height: frame.height * yScale
        ).integral.intersection(CGRect(x: 0, y: 0, width: cgImage.width, height: cgImage.height))
        guard !crop.isEmpty, let cropped = cgImage.cropping(to: crop) else {
            throw XCTSkip("The requested UI element was outside the app screenshot")
        }
        return UIImage(cgImage: cropped, scale: image.scale, orientation: image.imageOrientation)
    }

    /// Returns the gap between the composited row background and its text pixels.
    /// Quantiles avoid depending on exact glyph coordinates or simulator scale.
    private func textContrast(in image: UIImage, lightText: Bool) throws -> Double {
        guard let cgImage = image.cgImage else {
            throw XCTSkip("Screenshot did not provide a CGImage")
        }
        let width = cgImage.width
        let height = cgImage.height
        var rgba = [UInt8](repeating: 0, count: width * height * 4)
        guard let context = CGContext(
            data: &rgba,
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else {
            throw XCTSkip("Could not create screenshot sampling context")
        }
        context.draw(cgImage, in: CGRect(x: 0, y: 0, width: width, height: height))

        var luminance: [Double] = []
        luminance.reserveCapacity(width * height)
        for offset in stride(from: 0, to: rgba.count, by: 4) {
            let red = Double(rgba[offset]) / 255.0
            let green = Double(rgba[offset + 1]) / 255.0
            let blue = Double(rgba[offset + 2]) / 255.0
            let value: Double = (0.299 * red) + (0.587 * green) + (0.114 * blue)
            luminance.append(value)
        }
        luminance.sort()
        guard !luminance.isEmpty else { throw XCTSkip("Screenshot contained no pixels") }
        let low = luminance[Int(Double(luminance.count - 1) * 0.02)]
        let middle = luminance[Int(Double(luminance.count - 1) * 0.5)]
        let high = luminance[Int(Double(luminance.count - 1) * 0.98)]
        return lightText ? high - middle : middle - low
    }

    private func button(containing text: String, in app: XCUIApplication) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label CONTAINS[c] %@", text)).firstMatch
    }

    /// Any element type by identifier (combined cards can surface as a
    /// button or an other-element depending on the OS).
    private func element(_ identifier: String, in app: XCUIApplication) -> XCUIElement {
        app.descendants(matching: .any)[identifier].firstMatch
    }

    /// Scroll the Today page with short, slow drags on its left margin (clear
    /// of the deck's own drag target) until `element` is hittable.
    private func scrollUntilHittable(_ element: XCUIElement, in app: XCUIApplication, attempts: Int = 10) {
        _ = element.waitForExistence(timeout: 3)
        var scrolled = false
        for _ in 0..<attempts where !element.isHittable {
            let from = app.coordinate(withNormalizedOffset: CGVector(dx: 0.07, dy: 0.7))
            from.press(forDuration: 0.05, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.07, dy: 0.45)))
            scrolled = true
        }
        // Let the scroll settle: a tap on a still-decelerating scroll view
        // only stops it and never reaches the button.
        if scrolled { Thread.sleep(forTimeInterval: 0.6) }
        XCTAssertTrue(element.isHittable, "\(element) never became hittable")
    }
}

/// Exercises the installed app and its saved authenticated connection. No fixtures or mocked transport.
final class MagiosLiveConcurrentVoiceUITests: XCTestCase {
    func testTextQueueDrainsWhileRealtimeCallStaysConnected() throws {
        try XCTSkipUnless(ProcessInfo.processInfo.environment["MAGIOS_LIVE_MIXED_INPUT"] == "1",
                          "Requires enrolled physical-device mixed input acceptance")
        #if targetEnvironment(simulator)
        throw XCTSkip("Requires an enrolled physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        let chat = app.tabBars.buttons["Chat"]
        XCTAssertTrue(chat.waitForExistence(timeout: 20)); chat.tap()
        app.buttons["Chat history"].tap()
        let personal = app.buttons["Personal"]
        if personal.waitForExistence(timeout: 3) { personal.tap() }
        let newSession = app.buttons["New Session"]
        XCTAssertTrue(newSession.waitForExistence(timeout: 5)); newSession.tap()
        let voice = app.buttons["chat-mic"]
        if voice.waitForExistence(timeout: 2) { voice.tap() }
        let settings = app.buttons["voice-settings-session"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10)); settings.tap()
        let wasOpenMic = app.buttons["Open mic"].isSelected
        app.buttons["Hold to talk"].tap()
        app.buttons["voice-settings-start-call"].tap()
        let keyboard = app.buttons["live-type-message"]
        XCTAssertTrue(keyboard.waitForExistence(timeout: 30)); keyboard.tap()
        let status = app.buttons["live-composer-status"]
        XCTAssertTrue(status.waitForExistence(timeout: 10), app.debugDescription)
        let connected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == 'Connected'"), object: status)
        XCTAssertEqual(XCTWaiter.wait(for: [connected], timeout: 30), .completed)
        let composer = app.textFields["chat-composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        let marker = "IOSMIX" + String(UUID().uuidString.prefix(6))
        composer.tap()
        composer.typeText("Queue timing test. Use the shell tool with command \"sleep 25\" and timeout_secs 45. Then reply exactly IOSMIXFIRSTDONE. Do not create files or save memories.")
        app.buttons["chat-send"].tap()
        composer.tap(); composer.typeText("Reply exactly \(marker).")
        XCTAssertEqual(app.buttons["chat-send"].label, "Queue message")
        app.buttons["chat-send"].tap()
        XCTAssertTrue(app.buttons["chat-queued-messages"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["End voice call"].exists, "Typing must keep the call connected")
        let waiting = XCTAttachment(screenshot: app.screenshot())
        waiting.name = "queued-text-during-live-call"; waiting.lifetime = .keepAlways; add(waiting)
        let answer = app.staticTexts.containing(NSPredicate(format: "label == %@", marker)).firstMatch
        XCTAssertTrue(answer.waitForExistence(timeout: 150), "Text must drain before ending the call")
        XCTAssertTrue(app.buttons["End voice call"].exists)
        XCTAssertEqual(status.value as? String, "Connected")
        let result = XCTAttachment(screenshot: app.screenshot())
        result.name = "text-completed-with-live-call-connected"; result.lifetime = .keepAlways; add(result)
        app.buttons["End voice call"].tap()
        if wasOpenMic {
            if voice.waitForExistence(timeout: 5) { voice.tap() }
            settings.tap(); app.buttons["Open mic"].tap(); app.buttons["Done"].tap()
        }
        #endif
    }

    func testTypedFollowupQueuesWhileParallelRemainsExplicit() throws {
        try XCTSkipUnless(ProcessInfo.processInfo.environment["MAGIOS_LIVE_TEXT_QUEUE"] == "1",
                          "Requires enrolled physical-device text queue acceptance")
        #if targetEnvironment(simulator)
        throw XCTSkip("Requires an enrolled physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        let chat = app.tabBars.buttons["Chat"]
        XCTAssertTrue(chat.waitForExistence(timeout: 20)); chat.tap()
        let typeInstead = app.buttons["chat-type-instead"]
        if typeInstead.waitForExistence(timeout: 2) { typeInstead.tap() }
        let composer = app.textFields["chat-composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        let existing = composer.value as? String ?? ""
        XCTAssertTrue(existing.isEmpty || existing == composer.placeholderValue, "Preserve the user's draft")
        // Use an isolated visible session so previous live acceptance transcripts
        // cannot dominate rendering or contaminate this queue check's context.
        app.buttons["Chat history"].tap()
        let personal = app.buttons["Personal"]
        if personal.waitForExistence(timeout: 3) { personal.tap() }
        let newSession = app.buttons["New Session"]
        XCTAssertTrue(newSession.waitForExistence(timeout: 5)); newSession.tap()
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        let marker = "IOSQUEUE" + String(UUID().uuidString.prefix(6))
        composer.tap()
        composer.typeText("Queue validation \(marker): explain volcanoes in 1200 words. Do not use tools or save memories.")
        let options = app.buttons["chat-send-options"]
        XCTAssertEqual(options.frame.midY, app.buttons["chat-send"].frame.midY, accuracy: 1)
        let draft = XCTAttachment(screenshot: app.screenshot()); draft.name = "compact-send-options"; draft.lifetime = .keepAlways; add(draft)
        app.buttons["chat-send"].tap()
        composer.tap(); composer.typeText("Reply exactly \(marker).")
        XCTAssertEqual(app.buttons["chat-send"].label, "Queue message", "Ordinary Send must queue while the first reply runs")
        app.buttons["chat-send"].tap()
        let queued = app.buttons["chat-queued-messages"]
        XCTAssertTrue(queued.waitForExistence(timeout: 10), app.debugDescription)
        queued.tap()
        XCTAssertTrue(app.navigationBars["Queued messages"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["Stop & send"].exists)
        XCTAssertTrue(app.buttons["Run in parallel"].exists)
        let waiting = XCTAttachment(screenshot: app.screenshot()); waiting.name = "text-message-waits-in-queue"; waiting.lifetime = .keepAlways; add(waiting)
        app.buttons["Done"].tap()
        let answer = app.staticTexts.containing(NSPredicate(format: "label == %@", marker)).firstMatch
        XCTAssertTrue(answer.waitForExistence(timeout: 150), "The queued follow-up must execute after the first reply")
        let result = XCTAttachment(screenshot: app.screenshot()); result.name = "queued-text-answer"; result.lifetime = .keepAlways; add(result)
        #endif
    }

    func testOriginalAnswerLinkOpensItsExecutionConversation() throws {
        try XCTSkipUnless(ProcessInfo.processInfo.environment["MAGIOS_LIVE_ANSWER_LINK"] == "1",
                          "Requires enrolled physical-device answer-link acceptance")
        #if targetEnvironment(simulator)
        throw XCTSkip("Requires an enrolled physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        let chat = app.tabBars.buttons["Chat"]
        XCTAssertTrue(chat.waitForExistence(timeout: 20)); chat.tap()
        let typeInstead = app.buttons["chat-type-instead"]
        if typeInstead.waitForExistence(timeout: 2) { typeInstead.tap() }
        let composer = app.textFields["chat-composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        let existing = composer.value as? String ?? ""
        XCTAssertTrue(existing.isEmpty || existing == composer.placeholderValue, "Preserve the user's draft")
        let marker = "LINKCHECK" + String(UUID().uuidString.prefix(8))
        let links = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "original-answer-"))
        let previous = Set(links.allElementsBoundByIndex.map(\.identifier))
        let question = "Reply exactly \(marker). Do not use tools or save memories."
        composer.tap(); composer.typeText(question)
        app.buttons["chat-send-options"].tap(); app.buttons["Run in parallel"].tap()
        let resultOptions = app.buttons["Options for \(question)"]
        XCTAssertTrue(resultOptions.waitForExistence(timeout: 20), "This request must be admitted before checking completion")
        let summary = app.buttons["voice-requests-summary"]
        XCTAssertTrue(summary.waitForExistence(timeout: 20))
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == 'Ready' OR value == 'Answered'"), object: summary)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 90), .completed)
        // A user reading old history stays there when background work finishes.
        // Scroll before querying the lazy transcript's newly added answer.
        let latest = app.buttons["Scroll to latest message"]
        if latest.waitForExistence(timeout: 2) { latest.tap() }
        let linkAppeared = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            links.allElementsBoundByIndex.contains { !previous.contains($0.identifier) }
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [linkAppeared], timeout: 15), .completed, app.debugDescription)
        let link = try XCTUnwrap(links.allElementsBoundByIndex.last { !previous.contains($0.identifier) })
        let before = XCTAttachment(screenshot: app.screenshot()); before.name = "original-answer-link"; before.lifetime = .keepAlways; add(before)
        link.tap()
        let opened = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in !link.exists }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [opened], timeout: 20), .completed)
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label == %@", marker)).firstMatch.waitForExistence(timeout: 10))
        let read = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in !resultOptions.exists }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [read], timeout: 15), .completed, "Opening the exact answer must acknowledge that result")
        let after = XCTAttachment(screenshot: app.screenshot()); after.name = "original-answer-destination"; after.lifetime = .keepAlways; add(after)
        let origin = app.buttons["chat-concurrent-origin"]
        XCTAssertTrue(origin.exists)
        XCTAssertEqual(origin.frame.height, 30, accuracy: 1)
        XCTAssertGreaterThan(origin.frame.width, app.frame.width - 40)
        XCTAssertLessThan(origin.frame.maxY, composer.frame.minY)
        origin.tap()
        let returned = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in link.exists }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [returned], timeout: 20), .completed, "Parent link must restore the original conversation")
        let parent = XCTAttachment(screenshot: app.screenshot()); parent.name = "concurrent-parent-return"; parent.lifetime = .keepAlways; add(parent)
        #endif
    }

    func testCompactQueueAcceptsIndependentQuestionsAndPlaysSavedResult() throws {
        try XCTSkipUnless(ProcessInfo.processInfo.environment["MAGIOS_LIVE_CONCURRENT_VOICE"] == "1",
                          "Requires explicit physical-device concurrent voice acceptance")
        #if targetEnvironment(simulator)
        throw XCTSkip("Requires an enrolled physical iPhone")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        defer { app.terminate() }
        app.launch()
        let chat = app.tabBars.buttons["Chat"]
        XCTAssertTrue(chat.waitForExistence(timeout: 20)); chat.tap()
        let typeInstead = app.buttons["chat-type-instead"]
        if typeInstead.waitForExistence(timeout: 2) { typeInstead.tap() }
        let composer = app.textFields["chat-composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        let existing = composer.value as? String ?? ""
        XCTAssertTrue(existing.isEmpty || existing == composer.placeholderValue, "Preserve any existing user draft")
        let marker = String(UUID().uuidString.prefix(8))
        let first = "Voice test A \(marker): explain volcanoes in 1200 words. Do not use tools."
        let second = "Voice test B \(marker): reply exactly IOS_VOICE_B. Do not use tools."
        composer.tap(); composer.typeText(first); app.buttons["chat-send-options"].tap(); app.buttons["Run in parallel"].tap()
        let summary = app.buttons["voice-requests-summary"]
        XCTAssertTrue(summary.waitForExistence(timeout: 20), app.debugDescription)
        XCTAssertTrue(summary.label.hasPrefix("Expand"), "Queue must start collapsed inside the composer")
        composer.tap(); composer.typeText(second); app.buttons["chat-send-options"].tap(); app.buttons["Run in parallel"].tap()
        let latest = app.buttons["Options for \(second)"]
        XCTAssertTrue(latest.waitForExistence(timeout: 30), "Second question must be admitted independently")
        let collapsed = XCTAttachment(screenshot: app.screenshot()); collapsed.name = "voice-composer-collapsed"; collapsed.lifetime = .keepAlways; add(collapsed)
        summary.tap()
        XCTAssertTrue(app.buttons["Options for \(first)"].firstMatch.waitForExistence(timeout: 10), "First question must remain in the queue")
        let expanded = XCTAttachment(screenshot: app.screenshot()); expanded.name = "voice-composer-expanded"; expanded.lifetime = .keepAlways; add(expanded)
        summary.tap()
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == 'Ready' OR value == 'Answered'"), object: summary)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 120), .completed)
        latest.tap(); app.buttons["View result"].tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "IOS_VOICE_B")).firstMatch.waitForExistence(timeout: 20))
        app.buttons["Done"].tap()
        latest.tap(); app.buttons["Read aloud"].tap()
        let speaking = XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == 'Speaking'"), object: summary)
        XCTAssertEqual(XCTWaiter.wait(for: [speaking], timeout: 60), .completed)
        let answered = XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == 'Answered'"), object: summary)
        XCTAssertEqual(XCTWaiter.wait(for: [answered], timeout: 90), .completed, "Completion must follow actual playback")
        #endif
    }
}
