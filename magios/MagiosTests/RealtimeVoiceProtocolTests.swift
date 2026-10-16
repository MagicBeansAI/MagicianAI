import XCTest
@testable import Magician

final class RealtimeVoiceProtocolTests: XCTestCase {
    // MARK: - startPayload: engine × turn-boundary matrix
    //
    // These four cases are the standing regression guard for the bug where the
    // engine and the turn boundary were the same control: a Live call silently
    // ran on the cascaded engine because "hands-free" meant both "keep the mic
    // open" and "use the local STT→chat→TTS provider". If a future change
    // reconnects the two axes, exactly these tests fail.

    private func payload(_ engine: VoiceEngine, ptt: Bool) -> [String: Any] {
        RealtimeVoiceProtocol.startPayload(uiThreadId: "t1",
                                           realtimeProfile: "voice_realtime_openai_backend_mini",
                                           engine: engine, pttOn: ptt)
    }

    func testRealtimeEngineWithPttOffUsesServerVad() {
        let p = payload(.realtime, ptt: false)
        XCTAssertEqual(p["voice_mode"] as? String, "realtime")
        XCTAssertEqual(p["turn_boundary"] as? String, "server_vad")
        XCTAssertEqual(p["ui_thread_id"] as? String, "t1")
        XCTAssertEqual(p["realtime_profile"] as? String, "voice_realtime_openai_backend_mini")
        XCTAssertEqual(p["echo_cancellation"] as? Bool, true)
        XCTAssertNil(p["hands_free"])
    }

    func testRealtimeEngineWithPttOnUsesPushToTalk() {
        let p = payload(.realtime, ptt: true)
        XCTAssertEqual(p["voice_mode"] as? String, "realtime")
        XCTAssertEqual(p["turn_boundary"] as? String, "push_to_talk")
    }

    func testHandsFreeEngineWithPttOffUsesServerVad() {
        let p = payload(.handsFree, ptt: false)
        XCTAssertEqual(p["voice_mode"] as? String, "hands_free")
        XCTAssertEqual(p["turn_boundary"] as? String, "server_vad")
    }

    /// PTT is usable in the cascaded engine too: the backend's ptt.engage /
    /// ptt.release handlers are not engine-gated, so the client gates the PCM
    /// stream and still reports the boundary it is using.
    func testHandsFreeEngineWithPttOnUsesPushToTalk() {
        let p = payload(.handsFree, ptt: true)
        XCTAssertEqual(p["voice_mode"] as? String, "hands_free")
        XCTAssertEqual(p["turn_boundary"] as? String, "push_to_talk")
    }

    func testLiveTurnBoundaryUpdateUsesExistingControlProtocol() {
        XCTAssertEqual(
            RealtimeVoiceProtocol.turnBoundaryPayload(pttOn: false)["turn_boundary"] as? String,
            "server_vad"
        )
        XCTAssertEqual(
            RealtimeVoiceProtocol.turnBoundaryPayload(pttOn: true)["turn_boundary"] as? String,
            "push_to_talk"
        )
    }

    func testStartPayloadOmitsEmptyThread() {
        let p = RealtimeVoiceProtocol.startPayload(uiThreadId: "  ",
                                                   realtimeProfile: "x",
                                                   engine: .realtime, pttOn: false)
        XCTAssertNil(p["ui_thread_id"])   // empty → omitted so the backend defaults
        XCTAssertEqual(p["voice_mode"] as? String, "realtime")
    }

    func testStartPayloadCarriesPerCallAddressingOverride() {
        let p = RealtimeVoiceProtocol.startPayload(
            uiThreadId: "t1",
            realtimeProfile: "voice_realtime_openai_backend_mini",
            engine: .handsFree,
            pttOn: false,
            requireVoicePrefix: false
        )
        XCTAssertEqual(p["require_voice_prefix"] as? Bool, false)
        XCTAssertNil(payload(.realtime, ptt: false)["require_voice_prefix"])
    }

    func testStartAndTransitionPayloadsCarryActualScreenLockState() {
        let locked = RealtimeVoiceProtocol.startPayload(
            uiThreadId: "t1",
            realtimeProfile: "voice_realtime_openai_backend_mini",
            engine: .realtime,
            pttOn: false,
            screenLocked: true
        )
        XCTAssertEqual(locked["screen_locked"] as? Bool, true)
        XCTAssertEqual(
            RealtimeVoiceProtocol.screenStatePayload(locked: false)["locked"] as? Bool,
            false
        )
    }

    func testMediaSessionRegistrationCarriesLocalHandsFreeOverrides() {
        let body = RealtimeVoiceProtocol.mediaSessionRegistrationBody(
            includeHandsFreeAudio: true,
            audioProfile: "hands-free-fluid",
            audioStageOptions: ["vad": "silero", "streaming_stt": "qwen"]
        )
        XCTAssertEqual(body["audio_surface"] as? String, "hands_free")
        XCTAssertEqual(body["audio_profile"] as? String, "hands-free-fluid")
        let stages = body["audio_stage_options"] as? [String: String]
        XCTAssertEqual(stages?["vad"], "silero")
        XCTAssertEqual(stages?["streaming_stt"], "qwen")
    }

    func testMediaSessionRegistrationOmitsEmptyProfile() {
        let body = RealtimeVoiceProtocol.mediaSessionRegistrationBody(
            includeHandsFreeAudio: true,
            audioProfile: "",
            audioStageOptions: [:]
        )
        XCTAssertNil(body["audio_profile"])
    }

    func testRealtimeRegistrationDoesNotRequireTheHandsFreePipeline() {
        let body = RealtimeVoiceProtocol.mediaSessionRegistrationBody(
            includeHandsFreeAudio: false,
            audioProfile: "hands-free-fluid",
            audioStageOptions: ["vad": "silero"]
        )
        XCTAssertNil(body["audio_surface"])
        XCTAssertNil(body["audio_profile"])
        XCTAssertNil(body["audio_stage_options"])
    }
    func testRouteReadyRotatingRebindEndedError() {
        XCTAssertEqual(RealtimeVoiceProtocol.route(kind: "session.ready", payload: [:]), .ready)
        XCTAssertEqual(RealtimeVoiceProtocol.route(kind: "session.rotating", payload: [:]), .rotating)
        XCTAssertEqual(RealtimeVoiceProtocol.route(kind: "audio.rebind", payload: [:]), .rebind)
        XCTAssertEqual(RealtimeVoiceProtocol.route(kind: "session.ended", payload: [:]), .ended)
        XCTAssertEqual(RealtimeVoiceProtocol.route(kind: "session.error", payload: ["message": "x"]),
                       .error(message: "x", recoverable: false))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "session.error", payload: ["message": "x", "recoverable": true]),
            .error(message: "x", recoverable: true))
    }
    func testRouteTranscript() {
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "transcript.assistant", payload: ["text": "hi"]),
            .transcript(role: .assistant, text: "hi", itemID: nil, turnGeneration: nil))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "transcript.user", payload: [
                "text": "yo", "item_id": "turn-7", "turn_generation": 7
            ]),
            .transcript(role: .user, text: "yo", itemID: "turn-7", turnGeneration: 7))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "transcript.user.partial", payload: [
                "text": "hel", "item_id": "turn-7", "turn_generation": 7
            ]),
            .transcriptPartial(role: .user, text: "hel", itemID: "turn-7", turnGeneration: 7))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "transcript.user.ignored", payload: [
                "item_id": "turn-7", "turn_generation": 7, "reason": "address_prefix_required"
            ]),
            .transcriptIgnored(
                itemID: "turn-7", turnGeneration: 7, reason: "address_prefix_required"))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "transcript.user.cleared", payload: [
                "item_id": "turn-8", "turn_generation": 8, "reason": "no_final_transcript"
            ]),
            .transcriptCleared(
                itemID: "turn-8", turnGeneration: 8, reason: "no_final_transcript"))
    }

    func testRouteNativeTutorBlackboardHandoff() {
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(
                kind: "tutor.takeover.started",
                payload: [
                    "client_handoff": true,
                    "text": "@tutor #quick blackboard explain recursion",
                    "quick": true
                ]
            ),
            .tutorBlackboardRequested(
                text: "@tutor #quick blackboard explain recursion",
                quick: true
            )
        )
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(
                kind: "tutor.takeover.started",
                payload: ["client_handoff": false, "text": "@tutor explain recursion"]
            ),
            .ignore
        )
    }

    func testRouteGuidedFlowRejectionPreservesSpeechOwnership() {
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(
                kind: "tutor.takeover.failed",
                payload: [
                    "message": "Please unlock your screen to use Tutor.",
                    "backend_announced": true
                ]
            ),
            .guidedFlowRejected(
                message: "Please unlock your screen to use Tutor.",
                backendAnnounced: true
            )
        )
    }
    /// `interrupted` decides whether the client must flush its player queue —
    /// the server collapsed its echo window at the interrupt, so the flag has
    /// to survive routing exactly. A missing flag (older server) reads as a
    /// natural drain, which asks nothing of playback.
    func testRouteAssistantAudioEndedCarriesTheInterruptFlag() {
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(
                kind: "audio.output.ended",
                payload: ["response_id": "r1", "interrupted": true]
            ),
            .assistantAudioEnded(interrupted: true))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(
                kind: "audio.output.ended",
                payload: ["response_id": "r1", "interrupted": false]
            ),
            .assistantAudioEnded(interrupted: false))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "audio.output.ended", payload: [:]),
            .assistantAudioEnded(interrupted: false))
    }

    /// `interaction.status` is the only signal that the assistant is still on
    /// the request after an utterance ended. Only `in_progress` is working;
    /// `idle` and any spelling the client does not know read as not working,
    /// so a new server value can never leave "Working…" on screen.
    func testRouteInteractionStatusOnlyTrustsInProgress() {
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "interaction.status", payload: ["status": "in_progress"]),
            .interactionStatus(inProgress: true))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "interaction.status", payload: ["status": " IN_PROGRESS "]),
            .interactionStatus(inProgress: true))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "interaction.status", payload: ["status": "idle"]),
            .interactionStatus(inProgress: false))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "interaction.status", payload: ["status": "pondering"]),
            .interactionStatus(inProgress: false))
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(kind: "interaction.status", payload: [:]),
            .interactionStatus(inProgress: false))
    }

    /// The cascaded engine's barge-in takes the active response id before the
    /// provider's audio-done can match, so `response.interrupted` is the ONLY
    /// interrupt signal on that engine — it must route as one, not to ignore.
    func testResponseInterruptedRoutesAsAnInterruptedAudioEnd() {
        XCTAssertEqual(
            RealtimeVoiceProtocol.route(
                kind: "response.interrupted",
                payload: ["response_id": "r2"]
            ),
            .assistantAudioEnded(interrupted: true))
    }

    func testUnknownKindIsIgnored() {
        XCTAssertEqual(RealtimeVoiceProtocol.route(kind: "nonsense", payload: [:]), .ignore)
    }
    func testControlEnvelopeShape() {
        let env = RealtimeVoiceProtocol.envelopeText(kind: "ptt.engage", payload: [:])
        let obj = try! JSONSerialization.jsonObject(with: Data(env.utf8)) as! [String: Any]
        XCTAssertEqual(obj["kind"] as? String, "ptt.engage")
        XCTAssertNotNil(obj["payload"])
    }

    func testSessionReadyAddressingContract() {
        let addressing = RealtimeVoiceProtocol.addressing(from: [
            "addressing": [
                "required": true,
                "activation_phrases": ["Hey magical", "", 42],
                "follow_up_window_ms": 6_000
            ]
        ])
        XCTAssertEqual(
            addressing,
            .init(required: true, activationPhrases: ["Hey magical"], followUpWindowMs: 6_000)
        )
        XCTAssertEqual(RealtimeVoiceProtocol.addressing(from: [:]), .disabled)
    }

    /// The boundary is display-only and must fail to "unknown" rather than to
    /// "owner": an older backend sends no `boundary` block, and showing nothing
    /// is safe while implying the call is private is not.
    func testSessionReadyBoundaryContract() {
        let room = RealtimeVoiceProtocol.boundary(from: [
            "boundary": [
                "surface": "meeting",
                "audience": "untrusted",
                "agent_id": "envoy",
                "elevatable": false
            ]
        ])
        XCTAssertEqual(room?.surface, "meeting")
        XCTAssertEqual(room?.audience, "untrusted")
        XCTAssertEqual(room?.agentID, "envoy")
        XCTAssertEqual(room?.isUntrusted, true)

        // Never elevatable, whatever the wire says. The client does not carry a
        // value that could be used to argue the boundary is negotiable.
        let lying = RealtimeVoiceProtocol.boundary(from: [
            "boundary": [
                "surface": "meeting",
                "audience": "untrusted",
                "elevatable": true
            ]
        ])
        XCTAssertEqual(lying?.elevatable, false)

        // Absent or malformed -> unknown, not owner.
        XCTAssertNil(RealtimeVoiceProtocol.boundary(from: [:]))
        XCTAssertNil(RealtimeVoiceProtocol.boundary(from: ["boundary": ["surface": "meeting"]]))
        XCTAssertNil(RealtimeVoiceProtocol.boundary(from: ["boundary": ["audience": "  "]]))

        let owner = RealtimeVoiceProtocol.boundary(from: [
            "boundary": ["surface": "realtime_voice", "audience": "owner", "agent_id": "pa"]
        ])
        XCTAssertEqual(owner?.isUntrusted, false)
    }

    func testCaptionStatePromotesMatchingPartialWithoutDuplicatingTurn() {
        var state = RealtimeVoiceCaptionState()
        state.apply(.transcriptPartial(
            role: .user, text: "hel", itemID: "turn-1", turnGeneration: 1))
        state.apply(.transcriptPartial(
            role: .user, text: "hello", itemID: "turn-1", turnGeneration: 1))
        state.apply(.transcript(
            role: .user, text: "hello there", itemID: "turn-1", turnGeneration: 1))

        XCTAssertEqual(state.captions.count, 1)
        XCTAssertEqual(state.captions.first?.text, "hello there")
        XCTAssertEqual(state.captions.first?.isFinal, true)
        XCTAssertEqual(state.captions.first?.sourceItemID, "turn-1")
    }

    func testCaptionStateRemovesOnlyIgnoredPartial() {
        var state = RealtimeVoiceCaptionState()
        state.apply(.transcript(
            role: .assistant, text: "Ready", itemID: nil, turnGeneration: nil))
        state.apply(.transcriptPartial(
            role: .user, text: "background speech", itemID: "turn-2", turnGeneration: 2))
        state.apply(.transcriptIgnored(
            itemID: "turn-2", turnGeneration: 2, reason: "address_prefix_required"))

        XCTAssertEqual(state.captions.count, 1)
        XCTAssertEqual(state.captions.first?.role, .assistant)
        XCTAssertEqual(state.captions.first?.text, "Ready")
    }

    func testCaptionStateSilentlyRemovesOnlyClearedPartial() {
        var state = RealtimeVoiceCaptionState()
        state.apply(.transcript(
            role: .assistant, text: "Ready", itemID: nil, turnGeneration: nil))
        state.apply(.transcriptPartial(
            role: .user, text: "background speech", itemID: "turn-3", turnGeneration: 3))
        state.apply(.transcriptCleared(
            itemID: "turn-3", turnGeneration: 3, reason: "no_final_transcript"))

        XCTAssertEqual(state.captions.count, 1)
        XCTAssertEqual(state.captions.first?.role, .assistant)
        XCTAssertEqual(state.captions.first?.text, "Ready")
    }

    func testNewPartialSupersedesStaleUnfinishedCaption() {
        var state = RealtimeVoiceCaptionState()
        state.apply(.transcriptPartial(
            role: .user, text: "old", itemID: "turn-old", turnGeneration: 1))
        state.apply(.transcriptPartial(
            role: .user, text: "new", itemID: "turn-new", turnGeneration: 2))

        XCTAssertEqual(state.captions.count, 1)
        XCTAssertEqual(state.captions.first?.sourceItemID, "turn-new")
        XCTAssertEqual(state.captions.first?.text, "new")
    }

    func testDelayedFinalDoesNotDeleteNewerPartial() {
        var state = RealtimeVoiceCaptionState()
        state.apply(.transcriptPartial(
            role: .user, text: "first turn", itemID: "turn-a", turnGeneration: 1))
        state.apply(.transcriptPartial(
            role: .user, text: "second turn in progress", itemID: "turn-b", turnGeneration: 2))
        state.apply(.transcript(
            role: .user, text: "first turn final", itemID: "turn-a", turnGeneration: 1))

        XCTAssertEqual(state.captions.count, 2)
        XCTAssertEqual(state.captions[0].sourceItemID, "turn-b")
        XCTAssertEqual(state.captions[0].isFinal, false)
        XCTAssertEqual(state.captions[1].sourceItemID, "turn-a")
        XCTAssertEqual(state.captions[1].isFinal, true)
    }
}
