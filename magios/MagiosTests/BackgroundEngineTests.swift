import XCTest
@testable import Magician

@MainActor
private final class RefreshCallCounter {
    private(set) var count = 0
    func increment() { count += 1 }
}

/// The Dynamic Island task-id correlation gate — the fix that stopped the Live
/// Activity from applying whichever run's delta arrived last.
final class BackgroundEngineTests: XCTestCase {
    private var magiosRoot: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
    }

    func testUnboundRejectsEveryEvent() {
        XCTAssertFalse(BackgroundEngine.shouldApply(bound: nil, event: "task-1"))
        XCTAssertFalse(BackgroundEngine.shouldApply(bound: nil, event: nil))
    }

    func testBoundAppliesOnlyMatchingTask() {
        XCTAssertTrue(BackgroundEngine.shouldApply(bound: "task-1", event: "task-1"))
        XCTAssertFalse(BackgroundEngine.shouldApply(bound: "task-1", event: "task-2"))
        XCTAssertFalse(BackgroundEngine.shouldApply(bound: "task-1", event: nil))
    }

    func testOnlyTheCurrentInvocationOwnsLifecycleCallbacks() {
        let old = UUID()
        let current = UUID()

        XCTAssertTrue(BackgroundEngine.ownsLifecycle(current: current, expected: current))
        XCTAssertFalse(BackgroundEngine.ownsLifecycle(current: current, expected: old))
        XCTAssertFalse(BackgroundEngine.ownsLifecycle(current: nil, expected: old))
    }

    func testActivityDigestRespectsTheTwoWireOrderings() {
        let older = FeedItem(
            id: "old", kind: "step", timestamp: 1,
            title: "Older", content: nil, agentId: nil
        )
        let newer = FeedItem(
            id: "new", kind: "step", timestamp: 2,
            title: "Newer", content: nil, agentId: nil
        )

        let fullLog = BackgroundEngine.activityDigest(
            activityLog: [older, newer],
            recentActivity: []
        )
        let recentFallback = BackgroundEngine.activityDigest(
            activityLog: nil,
            recentActivity: [newer, older]
        )

        XCTAssertEqual(fullLog.latest?.id, "new")
        XCTAssertEqual(recentFallback.latest?.id, "new")
        XCTAssertEqual(fullLog.count, 2)
        XCTAssertEqual(recentFallback.count, 2)
    }

    func testTerminalActivityCopyNeverClaimsEveryEndWasSuccess() {
        XCTAssertEqual(
            BackgroundEngine.taskStatusText(status: "completed", latestActivity: "Last step"),
            "Done."
        )
        XCTAssertEqual(
            BackgroundEngine.taskStatusText(status: "failed", latestActivity: "Last step"),
            "Did not finish."
        )
        XCTAssertEqual(
            BackgroundEngine.taskStatusText(status: "cancelled", latestActivity: "Last step"),
            "Cancelled."
        )
        XCTAssertEqual(
            BackgroundEngine.taskStatusText(status: "running", latestActivity: "Last step"),
            "Last step"
        )
    }

    func testTerminalRealtimeStateEndsInsteadOfRacingAnUpdate() {
        XCTAssertEqual(
            BackgroundEngine.taskActivityMutation(isDone: false),
            .update
        )
        XCTAssertEqual(
            BackgroundEngine.taskActivityMutation(isDone: true),
            .end
        )
    }

    func testPersonalTeamDebugLiveActivityDoesNotRequestARemotePushToken() {
        XCTAssertFalse(MobilePushBuildSupport.remoteNotificationsEnabled)
        XCTAssertNil(BackgroundEngine.taskActivityPushType)
    }

    func testFrequentRemoteActivityUpdatesRemainDeclaredAcrossRegeneration() throws {
        let plistURL = magiosRoot.appendingPathComponent("Magios/Info.plist")
        let plistData = try Data(contentsOf: plistURL)
        let plist = try XCTUnwrap(
            PropertyListSerialization.propertyList(from: plistData, format: nil)
                as? [String: Any]
        )
        XCTAssertEqual(plist["NSSupportsLiveActivities"] as? Bool, true)
        XCTAssertEqual(plist["NSSupportsLiveActivitiesFrequentUpdates"] as? Bool, true)

        let projectSource = try String(
            contentsOf: magiosRoot.appendingPathComponent("project.yml"),
            encoding: .utf8
        )
        XCTAssertTrue(projectSource.contains("NSSupportsLiveActivitiesFrequentUpdates: true"))
    }

    func testPersonalTeamDebugSigningDoesNotClaimPaidPushEntitlement() throws {
        let defaultData = try Data(contentsOf: magiosRoot.appendingPathComponent(
            "Magios/Magican.entitlements"
        ))
        let defaultEntitlements = try XCTUnwrap(
            PropertyListSerialization.propertyList(from: defaultData, format: nil)
                as? [String: Any]
        )
        XCTAssertNil(defaultEntitlements["aps-environment"])

        let pushData = try Data(contentsOf: magiosRoot.appendingPathComponent(
            "Magios/MagicanPush.entitlements"
        ))
        let pushEntitlements = try XCTUnwrap(
            PropertyListSerialization.propertyList(from: pushData, format: nil)
                as? [String: Any]
        )
        XCTAssertEqual(pushEntitlements["aps-environment"] as? String, "$(APS_ENVIRONMENT)")

        let projectSource = try String(
            contentsOf: magiosRoot.appendingPathComponent("project.yml"),
            encoding: .utf8
        )
        XCTAssertTrue(projectSource.contains("Release:"))
        XCTAssertTrue(projectSource.contains("CODE_SIGN_ENTITLEMENTS: Magios/MagicanPush.entitlements"))
        XCTAssertTrue(projectSource.contains("SWIFT_ACTIVE_COMPILATION_CONDITIONS: $(inherited) MAGIOS_REMOTE_PUSH"))
    }

    func testPushRetriesOnlyTransientHTTPFailures() {
        XCTAssertTrue(MobilePushRegistrationClient.shouldRetryHTTPStatus(408))
        XCTAssertTrue(MobilePushRegistrationClient.shouldRetryHTTPStatus(429))
        XCTAssertTrue(MobilePushRegistrationClient.shouldRetryHTTPStatus(503))
        XCTAssertFalse(MobilePushRegistrationClient.shouldRetryHTTPStatus(400))
        XCTAssertFalse(MobilePushRegistrationClient.shouldRetryHTTPStatus(401))
        XCTAssertFalse(MobilePushRegistrationClient.shouldRetryHTTPStatus(404))
        XCTAssertFalse(MobilePushRegistrationClient.shouldRetryHTTPFailure(
            503,
            errorCode: "mobile_push_provider_not_configured"
        ))
        XCTAssertTrue(MobilePushRegistrationClient.shouldRetryHTTPFailure(
            503,
            errorCode: "temporarily_unavailable"
        ))
    }

    func testPushRegistrationResponseCarriesAnOpaqueCompareDeleteRevision() throws {
        let response = try JSONDecoder().decode(
            MobilePushRouteRegistration.self,
            from: Data(#"{"revision":42}"#.utf8)
        )

        XCTAssertEqual(response.revision, 42)
    }

    func testRuntimeConnectionChangeRetriesApplicationPushRegistration() throws {
        let source = try String(
            contentsOf: magiosRoot.appendingPathComponent("Magios/App.swift"),
            encoding: .utf8
        )
        let connectionHandler = try XCTUnwrap(
            source.range(of: ".magicianMobileConnectionDidChange")
        )
        let registration = try XCTUnwrap(
            source.range(
                of: "MobileAtAGlanceUpdates.shared.configureIfAllowed()",
                range: connectionHandler.lowerBound..<source.endIndex
            )
        )

        XCTAssertLessThan(connectionHandler.lowerBound, registration.lowerBound)
    }

    func testForegroundNotificationPresentationDoesNotWaitForTodayNetworkRefresh() throws {
        let source = try String(
            contentsOf: magiosRoot.appendingPathComponent("Magios/App.swift"),
            encoding: .utf8
        )
        let method = try XCTUnwrap(
            source.split(
                separator: "willPresent notification: UNNotification",
                maxSplits: 1
            ).last?.split(
                separator: "didReceive response: UNNotificationResponse",
                maxSplits: 1
            ).first
        )

        XCTAssertTrue(method.contains("Task { @MainActor in"))
        XCTAssertTrue(method.contains("return [.banner, .list, .sound]"))
        XCTAssertFalse(method.contains("_ = await MobileAtAGlanceUpdates.shared.handleRemotePayload(\n            notification"))
    }

    @MainActor
    func testRemoteNotificationBurstSharesOneGlanceRefresh() async {
        let gate = MobileGlanceRefreshGate()
        let calls = RefreshCallCounter()

        async let first = gate.run {
            calls.increment()
            await Task.yield()
            return true
        }
        async let second = gate.run {
            calls.increment()
            await Task.yield()
            return true
        }

        let firstResult = await first
        let secondResult = await second
        XCTAssertTrue(firstResult)
        XCTAssertTrue(secondResult)
        XCTAssertEqual(calls.count, 1)
    }

    func testLiveActivityTeardownDrainsRegistrationBeforeDeletingRoute() throws {
        let source = try String(
            contentsOf: magiosRoot.appendingPathComponent("Magios/BackgroundEngine.swift"),
            encoding: .utf8
        )
        let cancel = try XCTUnwrap(source.range(of: "pushTokenTask?.cancel()"))
        let drain = try XCTUnwrap(source.range(of: "await pushTokenTask.value"))
        let unregister = try XCTUnwrap(
            source.range(of: "await MobilePushRegistrationClient.unregisterWithRetry(")
        )

        XCTAssertLessThan(cancel.lowerBound, drain.lowerBound)
        XCTAssertLessThan(drain.lowerBound, unregister.lowerBound)
        XCTAssertTrue(source.contains("expectedRevision: revision"))
        XCTAssertFalse(source.contains("await MobilePushRegistrationClient.unregister(\n"))
    }

    func testIntentStopsUncorrelatedActivityOnEveryDispatchFailurePath() throws {
        let source = try String(
            contentsOf: magiosRoot.appendingPathComponent("MagiosIntents/MagiosIntents.swift"),
            encoding: .utf8
        )
        let intent = String(try XCTUnwrap(
            source.split(separator: "struct AskMagicianIntent", maxSplits: 1).last?
                .split(separator: "struct NewChatIntent", maxSplits: 1).first
        ))

        XCTAssertEqual(
            intent.components(
                separatedBy: "await BackgroundEngine.shared.stop(lifecycleID: activityLifecycleID)"
            ).count - 1,
            3,
            "non-2xx, missing task correlation, and transport failure must all tear down"
        )
        XCTAssertTrue(intent.contains("await BackgroundEngine.shared.bindTask("))
        XCTAssertTrue(intent.contains("lifecycleID: activityLifecycleID"))
    }

    func testUnavailableLiveActivityDoesNotStartInvisibleBackgroundTracking() throws {
        let source = try String(
            contentsOf: magiosRoot.appendingPathComponent("Magios/BackgroundEngine.swift"),
            encoding: .utf8
        )
        let start = String(try XCTUnwrap(
            source.split(separator: "func start(taskName:", maxSplits: 1).last?
                .split(separator: "func bindTask", maxSplits: 1).first
        ))

        XCTAssertTrue(start.contains("guard startLiveActivity(taskName: taskName) else"))
        XCTAssertTrue(start.contains("self.lifecycleID = nil"))
        XCTAssertTrue(start.contains("return nil"))
        XCTAssertLessThan(
            try XCTUnwrap(start.range(of: "guard startLiveActivity")).lowerBound,
            try XCTUnwrap(start.range(of: "playSilence")).lowerBound
        )
    }

    func testBackgroundTrackerDoesNotLogCompleteRealtimePayloads() throws {
        let source = try String(
            contentsOf: magiosRoot.appendingPathComponent("Magios/BackgroundEngine.swift"),
            encoding: .utf8
        )

        XCTAssertFalse(source.contains("Background WebSocket received: \\(text)"))
        XCTAssertTrue(source.contains("Background WebSocket received event: \\(eventType)"))
    }

    func testReusableBackgroundAudioGraphDoesNotReattachItsPlayerNode() throws {
        let source = try String(
            contentsOf: magiosRoot.appendingPathComponent("Magios/BackgroundEngine.swift"),
            encoding: .utf8
        )

        XCTAssertTrue(source.contains("if playerNode.engine == nil"))
        XCTAssertEqual(source.components(separatedBy: "engine.attach(playerNode)").count - 1, 1)
    }

    /// The ambient rail: `stop()` must ASK before it deactivates the shared audio
    /// session.
    ///
    /// This is reachable mid-window with no unusual user action — a Siri or
    /// App-Intent task dispatch starts the keepalive
    /// (`MagiosIntents.AskMagicianIntent.perform`) and completion stops it — and
    /// deactivating there is unrecoverable rather than rude: iOS refuses to
    /// reactivate a recording session from the background, so an armed window would
    /// not fail at that line but at the next wake word, off screen, with the orb
    /// still claiming the user is being heard (Apple DTS 826462, design §15).
    ///
    /// What is asserted is that the question is asked. Whether a session is active
    /// is not readable through any `AVAudioSession` API, so the observable property
    /// is the consultation — which is also the thing a future edit would delete.
    @MainActor
    func testStopConsultsTheAmbientRailBeforeReleasingTheSession() async {
        final class Asked { var count = 0 }
        let asked = Asked()
        let engine = BackgroundEngine()
        engine.ambientRail = AmbientRail(
            windowIsLive: {
                asked.count += 1
                return true
            },
            yield: { _ in
                XCTFail("a background task dispatch completing is not a decision to stop listening")
            }
        )

        engine.stop()
        // The rail is read on the main actor because `stop()` is also called from
        // the WebSocket delegate's queue, so the read is one hop away.
        for _ in 0 ..< 500 where asked.count == 0 { await Task.yield() }

        XCTAssertEqual(asked.count, 1, "stop() must not deactivate the shared session without checking for an armed window")
    }
}
