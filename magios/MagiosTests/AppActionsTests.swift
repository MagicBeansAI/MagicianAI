import XCTest
@testable import Magician

/// The in-app router for App-Intent / cross-surface navigation signals.
/// AppActions is a singleton; MagiosTests run serially, so before/after deltas
/// on the published counters are stable.
final class AppActionsTests: XCTestCase {
    private let a = AppActions()

    func testRequestNewChatBumpsID() {
        let before = a.newChatRequestID
        a.requestNewChat()
        XCTAssertEqual(a.newChatRequestID, before + 1)
    }

    func testRequestVoiceBumpsID() {
        let before = a.voiceRequestID
        a.requestVoice()
        XCTAssertEqual(a.voiceRequestID, before + 1)
        XCTAssertEqual(a.pendingVoiceLaunchMode, .configured)
        XCTAssertEqual(a.consumeVoiceLaunchMode(), .configured)
        XCTAssertNil(a.consumeVoiceLaunchMode())
    }

    func testConfiguredVoiceRequestResolvesCurrentDevicePreference() {
        XCTAssertEqual(VoiceLaunchMode.configured.resolved(using: .dictation), .dictation)
        XCTAssertEqual(VoiceLaunchMode.configured.resolved(using: .handsFree), .handsFree)
        XCTAssertEqual(VoiceLaunchMode.configured.resolved(using: .realtime), .realtime)
    }

    func testLegacyVoiceLaunchContractsRemainExecutableForInstalledAutomations() {
        XCTAssertEqual(SharedActions.configuredVoiceURL.absoluteString, "magican://voice")
        XCTAssertEqual(SharedActions.ambientURL.absoluteString, "magican://ambient")
        XCTAssertEqual(StartVoiceIntent().target, .configuredVoice)
        XCTAssertFalse(StartVoiceIntent.isDiscoverable)
    }

    func testAmbientTalkRequestIsLatchedUntilTheActivationConsumesIt() {
        XCTAssertFalse(a.consumeAmbientArmRequest())
        a.requestAmbientArm()
        XCTAssertTrue(a.consumeAmbientArmRequest())
        XCTAssertFalse(a.consumeAmbientArmRequest())
    }

    func testExplicitVoiceRequestOverridesDevicePreference() {
        XCTAssertEqual(VoiceLaunchMode.dictation.resolved(using: .realtime), .dictation)
        XCTAssertEqual(VoiceLaunchMode.handsFree.resolved(using: .dictation), .handsFree)
        XCTAssertEqual(VoiceLaunchMode.realtime.resolved(using: .handsFree), .realtime)
    }

    func testPrimaryAgentSiriIdentitySelectsMarkedPrimaryAndNormalizesAliases() throws {
        let payload = jsonData([
            "agents": [
                ["definition": [
                    "agent_id": "worker", "name": "Worker", "aliases": ["helper"]
                ]],
                ["definition": [
                    "agent_id": "primary-runtime",
                    "name": "Nova",
                    "aliases": [" nova ", "Sam", "sám", ""],
                    "is_primary": true
                ]]
            ],
            "total_count": 2
        ])

        let identity = try PrimaryAgentSiriIdentity.decodePrimary(from: payload)

        XCTAssertEqual(identity.agentID, "primary-runtime")
        XCTAssertEqual(identity.advertisedNames, ["Nova", "Sam"])
        XCTAssertEqual(identity.entities.map(\.spokenName), ["Nova", "Sam"])
        XCTAssertTrue(identity.entities.allSatisfy { $0.agentID == "primary-runtime" })
    }

    func testPrimaryAgentSiriIdentityFailsClosedWithoutMarkedPrimary() {
        let payload = jsonData([
            "agents": [["definition": ["agent_id": "worker", "name": "Worker"]]],
            "total_count": 1
        ])
        XCTAssertThrowsError(try PrimaryAgentSiriIdentity.decodePrimary(from: payload)) { error in
            XCTAssertEqual(error as? PrimaryAgentSiriIdentityError, .primaryAgentMissing)
        }
    }

    func testPrimaryAgentSiriIdentityCacheRoundTripsWithoutHardcodedFallback() throws {
        let suiteName = "AppActionsTests.siri.\(UUID().uuidString)"
        let store = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { store.removePersistentDomain(forName: suiteName) }
        let identity = PrimaryAgentSiriIdentity(
            agentID: "custom-primary", name: "Nova", aliases: ["N", "Nova"])

        XCTAssertTrue(PrimaryAgentSiriIdentityStore.save(identity, to: store))
        XCTAssertFalse(PrimaryAgentSiriIdentityStore.save(identity, to: store))
        XCTAssertEqual(PrimaryAgentSiriIdentityStore.load(from: store), identity)
    }

    func testSiriSettingsPresentsEveryRuntimeNameAsAnAppQualifiedPhrase() {
        let identity = PrimaryAgentSiriIdentity(
            agentID: "primary", name: "Nova", aliases: ["Sam", "nova"]
        )

        XCTAssertEqual(
            SiriPhrasePresentation.automaticPhrases(identity: identity),
            ["Ask Nova using Magican", "Ask Sam using Magican", "Ask Magican"]
        )
    }

    func testSiriSettingsOffersShortAndAIQualifiedPersonalNames() {
        let identity = PrimaryAgentSiriIdentity(
            agentID: "primary", name: "Nova", aliases: ["Sam"]
        )

        XCTAssertEqual(
            SiriPhrasePresentation.personalSuggestions(identity: identity),
            ["Ask Nova", "Ask Nova AI", "Ask Sam", "Ask Sam AI"]
        )
        XCTAssertEqual(
            SiriPhrasePresentation.initialPersonalPhrase(stored: "  My Sam  ", identity: identity),
            "My Sam"
        )
        XCTAssertEqual(
            SiriPhrasePresentation.initialPersonalPhrase(stored: "  ", identity: identity),
            "Ask Nova"
        )
    }

    // MARK: - Primary agent name equal to the app name

    func testSiriSettingsSuppressesTheRedundantAppQualifiedPrimaryPhrase() {
        // App Shortcut phrases must contain the app name, so an advertised name
        // equal to it would expand to "Ask Magican using Magican" alongside the
        // plain "Ask Magican" shortcut. The shipped identity has no aliases.
        let identity = PrimaryAgentSiriIdentity(
            agentID: "primary", name: "Magican", aliases: []
        )

        XCTAssertEqual(
            SiriPhrasePresentation.automaticPhrases(identity: identity, applicationName: "Magican"),
            ["Ask Magican"]
        )
    }

    func testSiriPersonalSuggestionsFallBackWhenTheOnlyNameIsTheAppName() {
        // An alias-less identity must still suggest something rather than an
        // empty list.
        let identity = PrimaryAgentSiriIdentity(
            agentID: "primary", name: "Magican", aliases: []
        )

        XCTAssertEqual(
            SiriPhrasePresentation.personalSuggestions(identity: identity, applicationName: "Magican"),
            ["Ask Magican", "Ask Magican AI"]
        )
    }

    @MainActor
    func testPrimaryAgentSiriAdvertiserFetchUsesScopedCrewInventory() async throws {
        defer { MockURLProtocol.handler = nil }
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.httpMethod, "GET")
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
            let components = try XCTUnwrap(URLComponents(url: request.url!, resolvingAgainstBaseURL: false))
            let query = Dictionary(uniqueKeysWithValues: (components.queryItems ?? []).map { ($0.name, $0.value ?? "") })
            XCTAssertNil(query["principal"])
            XCTAssertNil(query["workspace"])
            XCTAssertEqual(query["offset"], "0")
            XCTAssertEqual(query["limit"], "500")
            return (response(for: request), jsonData([
                "agents": [["definition": [
                    "agent_id": "runtime-primary", "name": "Nova",
                    "aliases": ["Spark"], "is_primary": true
                ]]],
                "total_count": 1
            ]))
        }

        let identity = try await PrimaryAgentSiriAdvertiser.fetchIdentity(session: makeMockSession())

        XCTAssertEqual(identity.agentID, "runtime-primary")
        XCTAssertEqual(identity.advertisedNames, ["Nova", "Spark"])
    }

    func testRequestAttentionSetsTargetAndBumps() {
        let before = a.attentionRequestID
        a.requestAttention(itemID: "item-9")
        XCTAssertEqual(a.attentionRequestID, before + 1)
        XCTAssertEqual(a.attentionTargetItemID, "item-9")
        a.consumeAttentionTarget()
        XCTAssertNil(a.attentionTargetItemID)
    }

    func testRequestTaskSetsTargetAndBumps() {
        let before = a.taskRequestID
        a.requestTask("task-9")
        XCTAssertEqual(a.taskRequestID, before + 1)
        XCTAssertEqual(a.taskTargetID, "task-9")
        a.consumeTaskTarget()
        XCTAssertNil(a.taskTargetID)
    }

    func testRequestThreadBumpsID() {
        let before = a.threadRequestID
        a.requestThread("thr-1")
        XCTAssertEqual(a.threadRequestID, before + 1)
        XCTAssertEqual(a.threadTargetID, "thr-1")
        a.consumeThreadTarget()
        XCTAssertNil(a.threadTargetID)
    }

    func testRequestTodayBumpsAndSetsActivity() {
        let before = a.todayRequestID
        a.requestToday(section: nil, activity: true)
        XCTAssertEqual(a.todayRequestID, before + 1)
        XCTAssertTrue(a.todayShowActivity)
        a.consumeTodayTarget()
        XCTAssertFalse(a.todayShowActivity)
    }

    // MARK: - Pending App-Group action routing

    func testConsumesNewChatPending() {
        SharedActions.setPending("new-chat")
        let before = a.newChatRequestID
        a.consumePendingIntentAction()
        XCTAssertEqual(a.newChatRequestID, before + 1)
    }

    func testEveryLegacySystemVoicePendingMigratesToAmbientTalk() {
        let legacyActions = [
            SharedActions.PendingAction.voiceDictation,
            SharedActions.PendingAction.voiceConfigured,
            SharedActions.PendingAction.legacyVoiceCall,
        ]

        for action in legacyActions {
            let before = a.voiceRequestID
            SharedActions.setPending(action)
            a.consumePendingIntentAction()

            XCTAssertEqual(a.voiceRequestID, before, "\(action) must not reopen one-shot Chat voice")
            XCTAssertNil(a.consumeVoiceLaunchMode())
            XCTAssertTrue(a.consumeAmbientArmRequest(), "\(action) must enter the durable Talk lifecycle")
            XCTAssertFalse(a.consumeAmbientArmRequest(), "the migrated request remains one-shot at the handoff")
        }
    }

    func testRequestBlackboardTutorBumpsID() {
        let before = a.tutorBlackboardRequestID
        a.requestBlackboardTutor()
        XCTAssertEqual(a.tutorBlackboardRequestID, before + 1)
    }

    func testConsumesBlackboardTutorPending() {
        SharedActions.setPending("tutor-blackboard")
        let before = a.tutorBlackboardRequestID
        a.consumePendingIntentAction()
        XCTAssertEqual(a.tutorBlackboardRequestID, before + 1)
    }

    func testUnknownPendingIsNoop() {
        SharedActions.setPending("nonsense")
        let nc = a.newChatRequestID, v = a.voiceRequestID
        a.consumePendingIntentAction()
        XCTAssertEqual(a.newChatRequestID, nc)
        XCTAssertEqual(a.voiceRequestID, v)
    }

    func testConsumePendingClearsAfterRead() {
        SharedActions.setPending(SharedActions.PendingAction.voiceConfigured)
        XCTAssertEqual(SharedActions.consumePending(), SharedActions.PendingAction.voiceConfigured)
        XCTAssertNil(SharedActions.consumePending())   // one-shot
    }

    func testConsumesPendingThreadTarget() {
        SharedActions.setPendingThread("thread-from-share")
        let before = a.threadRequestID
        a.consumePendingIntentAction()
        XCTAssertEqual(a.threadRequestID, before + 1)
        XCTAssertEqual(a.threadTargetID, "thread-from-share")
        a.consumeThreadTarget()
    }

    func testConsumesPendingTaskTarget() {
        SharedActions.setPendingTask("task-from-share")
        let before = a.taskRequestID
        a.consumePendingIntentAction()
        XCTAssertEqual(a.taskRequestID, before + 1)
        XCTAssertEqual(a.taskTargetID, "task-from-share")
        a.consumeTaskTarget()
    }
}
