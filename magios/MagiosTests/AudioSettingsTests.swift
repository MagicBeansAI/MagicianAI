import XCTest
@testable import Magician

/// The STT/TTS engine enums that back the composer's voice menu.
final class AudioSettingsTests: XCTestCase {
    func testSTTSourceLabels() {
        XCTAssertEqual(STTSource.auto.label, "Auto (iPhone, backend fallback)")
        XCTAssertEqual(STTSource.onDevice.label, "This iPhone · Apple Speech")
        XCTAssertEqual(STTSource.cloud.label, "Backend host")
    }

    func testSTTSourcePreferences() {
        XCTAssertTrue(STTSource.auto.prefersOnDevice)
        XCTAssertTrue(STTSource.onDevice.prefersOnDevice)
        XCTAssertFalse(STTSource.cloud.prefersOnDevice)
        XCTAssertTrue(STTSource.auto.allowsCloud)
        XCTAssertFalse(STTSource.onDevice.allowsCloud)
        XCTAssertTrue(STTSource.cloud.allowsCloud)
    }

    func testSTTSourceRawValueRoundTrip() {
        XCTAssertEqual(STTSource.onDevice.rawValue, "on_device")
        XCTAssertEqual(STTSource(rawValue: "on_device"), .onDevice)
        XCTAssertEqual(STTSource(rawValue: "auto"), .auto)
        XCTAssertNil(STTSource(rawValue: "garbage"))
    }

    func testTTSEngineLabels() {
        XCTAssertEqual(TTSEngine.onDevice.label, "This iPhone · Apple Voice")
        XCTAssertEqual(TTSEngine.magician.label, "Backend host")
    }

    func testTTSEngineRawValueRoundTrip() {
        XCTAssertEqual(TTSEngine.onDevice.rawValue, "on_device")
        XCTAssertEqual(TTSEngine.magician.rawValue, "magician")
        XCTAssertEqual(TTSEngine(rawValue: "magician"), .magician)
    }

    func testAllCasesCounts() {
        XCTAssertEqual(STTSource.allCases.count, 3)
        XCTAssertEqual(TTSEngine.allCases.count, 2)
        XCTAssertEqual(SystemVoiceLaunchMode.allCases.count, 3)
        XCTAssertEqual(AmbientVoiceMode.allCases.count, 3)
    }

    func testChatDictationArchiveRequiresExplicitOptInOnFreshInstall() {
        XCTAssertFalse(AudioSettings.archiveChatDictationDefault)
    }

    func testSystemVoiceLaunchModesHaveStableWireValuesAndCopy() {
        XCTAssertEqual(SystemVoiceLaunchMode.dictation.rawValue, "dictation")
        XCTAssertEqual(SystemVoiceLaunchMode.handsFree.rawValue, "hands_free")
        XCTAssertEqual(SystemVoiceLaunchMode.realtime.rawValue, "realtime")
        XCTAssertEqual(SystemVoiceLaunchMode.handsFree.label, "Hands-free")
        XCTAssertFalse(SystemVoiceLaunchMode.realtime.detail.isEmpty)
    }

    func testAmbientVoiceModeSeedPreservesEveryBackendConversationMode() {
        XCTAssertEqual(
            AudioSettings.ambientVoiceModeSeed(backendVoiceMode: "realtime"),
            .realtime
        )
        XCTAssertEqual(
            AudioSettings.ambientVoiceModeSeed(backendVoiceMode: " LIVE "),
            .realtime
        )
        XCTAssertEqual(
            AudioSettings.ambientVoiceModeSeed(backendVoiceMode: "hands_free"),
            .handsFree
        )
        XCTAssertEqual(
            AudioSettings.ambientVoiceModeSeed(backendVoiceMode: "recording"),
            .dictation
        )
        XCTAssertEqual(
            AudioSettings.ambientVoiceModeSeed(backendVoiceMode: "DICTATION"),
            .dictation
        )
        XCTAssertEqual(
            AudioSettings.ambientVoiceModeSeed(backendVoiceMode: nil),
            .handsFree
        )
    }

    func testAmbientModesOnlyExposeStreamingEnginesWhenTheyHaveOne() {
        XCTAssertNil(AmbientVoiceMode.dictation.streamingEngine)
        XCTAssertEqual(AmbientVoiceMode.handsFree.streamingEngine, .handsFree)
        XCTAssertEqual(AmbientVoiceMode.realtime.streamingEngine, .realtime)
        XCTAssertEqual(AmbientVoiceMode.dictation.rawValue, "dictation")
    }

    func testRealtimeProfileDistinguishesAssistantFromTranslation() {
        let assistant = RealtimeVoiceProfileOption(
            id: "voice_realtime_gemini_live",
            label: "Gemini 3.1 Flash Live",
            provider: "gemini_live",
            model: "gemini-3.1-flash-live-preview",
            topology: "backend_proxied",
            mode: "assistant",
            turnDetectionMode: "none",
            available: true,
            unavailableReason: nil
        )
        let translation = RealtimeVoiceProfileOption(
            id: "voice_realtime_gemini_translate_en",
            label: "Gemini 3.5 Live Translate (English)",
            provider: "gemini_live",
            model: "gemini-3.5-live-translate-preview",
            topology: "backend_proxied",
            mode: "translation",
            turnDetectionMode: "server_vad",
            available: true,
            unavailableReason: nil
        )
        XCTAssertFalse(assistant.isTranslation)
        XCTAssertTrue(translation.isTranslation)
        XCTAssertTrue(assistant.isSupportedByNativeClient)
        let browserOnly = RealtimeVoiceProfileOption(
            id: "voice_realtime_default",
            label: "GPT Realtime 2.1 Mini",
            provider: "openai_realtime",
            model: "gpt-realtime-2.1-mini",
            topology: "direct_peer_to_peer",
            mode: "assistant",
            turnDetectionMode: "server_vad",
            available: true,
            unavailableReason: nil
        )
        XCTAssertFalse(browserOnly.isSupportedByNativeClient)
    }

    func testRealtimeProfileSelectionKeepsAnAvailableNativeChoice() {
        let profiles = realtimeProfiles()
        XCTAssertEqual(
            AudioSettings.resolveRealtimeVoiceProfile(
                from: profiles,
                selectedID: "voice_realtime_gemini_live",
                backendDefaultID: "voice_realtime_default"
            ),
            "voice_realtime_gemini_live"
        )
    }

    func testRealtimeProfileSelectionReplacesStaleOrBrowserOnlyChoice() {
        XCTAssertEqual(
            AudioSettings.resolveRealtimeVoiceProfile(
                from: realtimeProfiles(),
                selectedID: "voice_realtime_default",
                backendDefaultID: "voice_realtime_default"
            ),
            "voice_realtime_openai_backend_mini"
        )
    }

    func testRealtimeProfileOrderingPutsNativeDefaultEquivalentFirst() {
        let ordered = AudioSettings.orderedNativeRealtimeProfiles(
            from: realtimeProfiles(),
            selectedID: nil,
            backendDefaultID: "voice_realtime_default"
        )
        XCTAssertEqual(
            ordered.map(\.id),
            ["voice_realtime_openai_backend_mini", "voice_realtime_gemini_live"]
        )
    }

    func testRealtimeProfileOrderingKeepsUserChoiceFirstAndDefaultSecond() {
        let ordered = AudioSettings.orderedNativeRealtimeProfiles(
            from: realtimeProfiles(),
            selectedID: "voice_realtime_gemini_live",
            backendDefaultID: "voice_realtime_default"
        )
        XCTAssertEqual(
            ordered.map(\.id),
            ["voice_realtime_gemini_live", "voice_realtime_openai_backend_mini"]
        )
    }

    func testRealtimeProfileSelectionReturnsNilWhenNoNativeProfileIsAvailable() {
        let unavailable = realtimeProfiles().map {
            RealtimeVoiceProfileOption(
                id: $0.id,
                label: $0.label,
                provider: $0.provider,
                model: $0.model,
                topology: $0.topology,
                mode: $0.mode,
                turnDetectionMode: $0.turnDetectionMode,
                available: false,
                unavailableReason: "not configured"
            )
        }
        XCTAssertNil(
            AudioSettings.resolveRealtimeVoiceProfile(
                from: unavailable,
                selectedID: "voice_realtime_openai_backend_mini",
                backendDefaultID: "voice_realtime_default"
            )
        )
    }

    func testTranslationForcesOpenMicOnlyOnRealtimeEngine() {
        let translation = RealtimeVoiceProfileOption(
            id: "translate",
            label: "Translate",
            provider: "gemini_live",
            model: "gemini-live-translate",
            topology: "backend_proxied",
            mode: "translation",
            turnDetectionMode: "server_vad",
            available: true,
            unavailableReason: nil
        )
        XCTAssertFalse(AudioSettings.resolveLiveVoicePttOn(
            requested: true,
            engine: .realtime,
            profile: translation
        ))
        XCTAssertTrue(AudioSettings.resolveLiveVoicePttOn(
            requested: true,
            engine: .handsFree,
            profile: translation
        ))
    }

    func testAssistantProfilePreservesIOSPushToTalkChoice() {
        let assistant = realtimeProfiles().first { $0.id == "voice_realtime_openai_backend_mini" }
        XCTAssertTrue(AudioSettings.resolveLiveVoicePttOn(
            requested: true,
            engine: .realtime,
            profile: assistant
        ))
        XCTAssertFalse(AudioSettings.resolveLiveVoicePttOn(
            requested: false,
            engine: .realtime,
            profile: assistant
        ))
    }

    func testBackendCatalogSeedsPTTFromSelectedProfileDefault() {
        let profile = realtimeProfiles().first {
            $0.id == "voice_realtime_openai_backend_mini"
        }
        XCTAssertTrue(profile?.defaultsToPushToTalk == true)
        XCTAssertFalse(realtimeProfiles()[0].defaultsToPushToTalk)
    }

    func testProviderCatalogParserKeepsBackendDefaults() {
        let catalog = AudioSettings.realtimeVoiceCatalog(from: [
            "realtime_voice_default_profile": "voice_realtime_default",
            "realtime_voice_profiles": [[
                "profile_id": "voice_realtime_default",
                "label": "GPT Realtime",
                "provider": "openai_realtime",
                "model": "gpt-realtime-2.1-mini",
                "topology": "direct_peer_to_peer",
                "mode": "assistant",
                "turn_detection_mode": "server_vad",
                "transcription_model": "local",
                "transcription_fallback_model": "whisper-1",
                "available": true
            ]]
        ])
        XCTAssertEqual(catalog.defaultProfileID, "voice_realtime_default")
        XCTAssertEqual(catalog.profiles.first?.turnDetectionMode, "server_vad")
        XCTAssertEqual(catalog.profiles.first?.transcriptionModel, "local")
        XCTAssertEqual(catalog.profiles.first?.transcriptionFallbackModel, "whisper-1")
        XCTAssertEqual(catalog.profiles.first?.transcriptionLabel, "Parallel STT · Backend host")
    }

    func testBackendAudioOptionsNameTheirExecutionLocation() {
        let mac = NativeAudioStageOption(
            id: "macos-speech", stage: .streamingSTT, providerID: "macos-speech",
            engineID: "macos_system", modelID: "system_default", label: "macOS Speech",
            available: true, unavailableReason: nil
        )
        let fluid = NativeAudioStageOption(
            id: "fluid-parakeet", stage: .streamingSTT, providerID: "fluid-parakeet",
            engineID: "fluid_audio", modelID: "parakeet", label: "FluidAudio Parakeet",
            available: true, unavailableReason: nil
        )
        let online = NativeAudioStageOption(
            id: "openai", stage: .recordingSTT, providerID: "openai",
            engineID: "online", modelID: "gpt-transcribe", label: "OpenAI",
            available: true, unavailableReason: nil
        )

        XCTAssertEqual(mac.displayLabel, "Mac host · macOS Speech")
        XCTAssertEqual(fluid.displayLabel, "Mac host · FluidAudio Parakeet")
        XCTAssertEqual(online.displayLabel, "Online · OpenAI")
    }

    func testNativeAudioCatalogKeepsSurfaceProfilesAndOnlyConfiguredStages() {
        let catalog = AudioSettings.nativeAudioCatalog(from: [
            "default_surface_profiles": [
                "dictation": "dictation-local",
                "hands_free": "hands-free-local"
            ],
            "surface_profiles": [
                "dictation-local": [
                    "surface": "dictation",
                    "recording_stt": ["enabled": true, "providers": ["stt-local"]],
                    "tts": ["enabled": true, "providers": ["tts-local"]]
                ],
                "hands-free-local": [
                    "surface": "hands_free",
                    "vad": ["enabled": true, "providers": ["vad-local"]],
                    "streaming_stt": ["enabled": true, "providers": ["stream-local"]],
                    "diarization": ["enabled": false, "providers": ["speakers-local"]],
                    "tts": ["enabled": true, "providers": ["tts-local"]]
                ]
            ],
            "stages": [
                "vad": [[
                    "option_id": "vad-option", "provider_id": "vad-local",
                    "engine_id": "fluid", "model_id": "vad", "label": "Local VAD",
                    "availability": "available"
                ]],
                "recording_stt": [[
                    "option_id": "stt-option", "provider_id": "stt-local",
                    "engine_id": "fluid", "model_id": "stt", "label": "Local STT",
                    "availability": "available"
                ]],
                "streaming_stt": [[
                    "option_id": "stream-option", "provider_id": "stream-local",
                    "engine_id": "fluid", "model_id": "stream", "label": "Local Stream",
                    "availability": "available"
                ]],
                "tts": [[
                    "option_id": "tts-option", "provider_id": "tts-local",
                    "engine_id": "fluid", "model_id": "voice", "label": "Local Voice",
                    "availability": "available"
                ]]
            ]
        ])

        XCTAssertEqual(catalog.defaultProfiles[.dictation], "dictation-local")
        let dictation = catalog.profiles.first { $0.id == "dictation-local" }
        XCTAssertEqual(dictation?.enabledStages, Set([.recordingSTT, .tts]))
        let handsFree = catalog.profiles.first { $0.id == "hands-free-local" }
        XCTAssertTrue(handsFree?.enabledStages.contains(.vad) == true)
        XCTAssertFalse(handsFree?.enabledStages.contains(.diarization) == true)
        XCTAssertEqual(catalog.stageOptions[.recordingSTT]?.first?.id, "stt-option")
        XCTAssertTrue(AudioSettings.handsFreeProfileAvailable(
            handsFree!,
            stageOptions: catalog.stageOptions
        ))

        var unavailableOptions = catalog.stageOptions
        unavailableOptions[.tts] = [NativeAudioStageOption(
            id: "tts-option",
            stage: .tts,
            providerID: "tts-local",
            engineID: "fluid",
            modelID: "voice",
            label: "Local Voice",
            available: false,
            unavailableReason: "engine disabled"
        )]
        XCTAssertFalse(AudioSettings.handsFreeProfileAvailable(
            handsFree!,
            stageOptions: unavailableOptions
        ))
    }

    func testUnavailableCatalogOptionRemainsAPersistedSelection() {
        let profile = NativeAudioProfileOption(
            id: "dictation-local",
            surface: .dictation,
            enabledStages: [.recordingSTT],
            providersByStage: [.recordingSTT: ["stt-local"]]
        )
        let unavailable = NativeAudioStageOption(
            id: "stt-option",
            stage: .recordingSTT,
            providerID: "stt-local",
            engineID: "fluid",
            modelID: "stt",
            label: "Local STT",
            available: false,
            unavailableReason: "engine restarting"
        )

        let retained = AudioSettings.retainedStageSelections(
            [.recordingSTT: unavailable.id, .tts: "stale-option"],
            profile: profile,
            stageOptions: [.recordingSTT: [unavailable]]
        )

        XCTAssertEqual(retained, [.recordingSTT: unavailable.id])
    }

    func testHandsFreeModeCanRecoverThroughAnotherAvailableProfile() {
        let unavailable = handsFreeProfile(id: "hands-free-a", provider: "provider-a")
        let available = handsFreeProfile(id: "hands-free-b", provider: "provider-b")
        let options = Dictionary(uniqueKeysWithValues: [
            NativeAudioStage.vad,
            .streamingSTT,
            .tts
        ].map { stage in
            (stage, [
                nativeStageOption(stage: stage, provider: "provider-a", available: false),
                nativeStageOption(stage: stage, provider: "provider-b", available: true)
            ])
        })

        XCTAssertFalse(AudioSettings.handsFreeProfileAvailable(
            unavailable,
            stageOptions: options
        ))
        XCTAssertTrue(AudioSettings.hasAvailableNativeProfile(
            for: .handsFree,
            profiles: [unavailable, available],
            stageOptions: options
        ))
    }

    func testNativePreferenceBootstrapRejectsErrorStatusJSON() throws {
        let payload = try JSONSerialization.data(withJSONObject: [
            "surface_profiles": ["dictation": "dictation-fluid"]
        ])
        let url = URL(string: "https://ios.example.com/api/magician/v2/media/preferences")!
        let unauthorized = HTTPURLResponse(
            url: url,
            statusCode: 401,
            httpVersion: nil,
            headerFields: nil
        )!
        XCTAssertNil(AudioSettings.successfulJSONObject(data: payload, response: unauthorized))

        let success = HTTPURLResponse(
            url: url,
            statusCode: 200,
            httpVersion: nil,
            headerFields: nil
        )!
        XCTAssertEqual(
            AudioSettings.successfulJSONObject(data: payload, response: success)?["surface_profiles"] as? [String: String],
            ["dictation": "dictation-fluid"]
        )
    }

    func testMediaPreferenceBootstrapRejectsMalformedSuccessPayload() throws {
        let url = URL(string: "https://ios.example.com/api/magician/v2/media/preferences")!
        let response = HTTPURLResponse(
            url: url,
            statusCode: 200,
            httpVersion: nil,
            headerFields: nil
        )!
        let valid = try JSONSerialization.data(withJSONObject: [
            "schema_version": 3,
            "auto_speak": false,
            "voice_mode": "recording",
            "require_voice_prefix": true,
            "surface_profiles": ["dictation": "dictation-local"],
            "surface_stage_options": [
                "dictation": ["recording_stt": "stt-local"]
            ]
        ])
        XCTAssertNotNil(AudioSettings.successfulMediaPreferencesJSONObject(
            data: valid,
            response: response
        ))

        let errorEnvelope = try JSONSerialization.data(withJSONObject: [
            "error": "temporarily_unavailable"
        ])
        XCTAssertNil(AudioSettings.successfulMediaPreferencesJSONObject(
            data: errorEnvelope,
            response: response
        ))

        let partial = try JSONSerialization.data(withJSONObject: [
            "schema_version": 3,
            "auto_speak": false
        ])
        XCTAssertNil(AudioSettings.successfulMediaPreferencesJSONObject(
            data: partial,
            response: response
        ))
    }

    func testProviderBootstrapRejectsMalformedSuccessPayload() throws {
        let url = URL(string: "https://ios.example.com/api/magician/v2/media/providers")!
        let response = HTTPURLResponse(
            url: url,
            statusCode: 200,
            httpVersion: nil,
            headerFields: nil
        )!
        let valid = try JSONSerialization.data(withJSONObject: [
            "audio_revision": "revision-1",
            "stages": [
                "vad": [[
                    "option_id": "vad-option",
                    "provider_id": "vad-local",
                    "engine_id": "fluid",
                    "model_id": "vad",
                    "label": "Local VAD",
                    "availability": "available"
                ]]
            ],
            "surface_profiles": [
                "hands-free-local": [
                    "surface": "hands_free",
                    "vad": ["enabled": true, "providers": ["vad-local"]],
                    "recording_stt": ["enabled": false],
                    "streaming_stt": ["enabled": false],
                    "diarization": ["enabled": false],
                    "tts": ["enabled": false]
                ]
            ],
            "default_surface_profiles": ["hands_free": "hands-free-local"],
            "engines": [:],
            "hands_free_voice": false
        ])
        XCTAssertNotNil(AudioSettings.successfulProviderCatalogJSONObject(
            data: valid,
            response: response
        ))

        let partial = try JSONSerialization.data(withJSONObject: [
            "audio_revision": "revision-1",
            "stages": [:],
            "surface_profiles": [:]
        ])
        XCTAssertNil(AudioSettings.successfulProviderCatalogJSONObject(
            data: partial,
            response: response
        ))

        let malformedProfile = try JSONSerialization.data(withJSONObject: [
            "audio_revision": "revision-1",
            "stages": [:],
            "surface_profiles": [
                "hands-free-local": ["surface": "hands_free"]
            ],
            "default_surface_profiles": ["hands_free": "hands-free-local"],
            "engines": [:],
            "hands_free_voice": false
        ])
        XCTAssertNil(AudioSettings.successfulProviderCatalogJSONObject(
            data: malformedProfile,
            response: response
        ))
    }

    func testNativePreferenceSeedPreservesScopedProfileAndStageChoices() {
        let seed = AudioSettings.nativePreferenceSeed(from: [
            "surface_profiles": ["dictation": "dictation-fluid"],
            "surface_stage_options": [
                "dictation": ["recording_stt": "qwen-stt", "tts": "kokoro-tts"],
                "hands_free": ["vad": "silero-vad"]
            ]
        ])
        XCTAssertEqual(seed.profiles[.dictation], "dictation-fluid")
        XCTAssertEqual(seed.stageOptions[.dictation]?[.recordingSTT], "qwen-stt")
        XCTAssertEqual(seed.stageOptions[.dictation]?[.tts], "kokoro-tts")
        XCTAssertEqual(seed.stageOptions[.handsFree]?[.vad], "silero-vad")
    }

    private func realtimeProfiles() -> [RealtimeVoiceProfileOption] {
        [
            RealtimeVoiceProfileOption(
                id: "voice_realtime_default",
                label: "GPT Realtime 2.1 Mini",
                provider: "openai_realtime",
                model: "gpt-realtime-2.1-mini",
                topology: "direct_peer_to_peer",
                mode: "assistant",
                turnDetectionMode: "server_vad",
                available: true,
                unavailableReason: nil
            ),
            RealtimeVoiceProfileOption(
                id: "voice_realtime_openai_backend_mini",
                label: "GPT Realtime 2.1 Mini",
                provider: "openai_realtime_backend",
                model: "gpt-realtime-2.1-mini",
                topology: "backend_proxied",
                mode: "assistant",
                turnDetectionMode: "none",
                available: true,
                unavailableReason: nil
            ),
            RealtimeVoiceProfileOption(
                id: "voice_realtime_gemini_live",
                label: "Gemini 3.1 Flash Live",
                provider: "gemini_live",
                model: "gemini-3.1-flash-live-preview",
                topology: "backend_proxied",
                mode: "assistant",
                turnDetectionMode: "none",
                available: true,
                unavailableReason: nil
            )
        ]
    }

    private func handsFreeProfile(id: String, provider: String) -> NativeAudioProfileOption {
        NativeAudioProfileOption(
            id: id,
            surface: .handsFree,
            enabledStages: [.vad, .streamingSTT, .tts],
            providersByStage: [
                .vad: [provider],
                .streamingSTT: [provider],
                .tts: [provider]
            ]
        )
    }

    private func nativeStageOption(
        stage: NativeAudioStage,
        provider: String,
        available: Bool
    ) -> NativeAudioStageOption {
        NativeAudioStageOption(
            id: "\(provider)-\(stage.rawValue)",
            stage: stage,
            providerID: provider,
            engineID: "fluid",
            modelID: stage.rawValue,
            label: "\(provider) \(stage.label)",
            available: available,
            unavailableReason: available ? nil : "engine unavailable"
        )
    }

    func testVoicePrefixPreferenceBodyUsesScopedBackendField() {
        let body = AudioSettings.voicePrefixPreferenceBody(
            required: false,
            principal: "person",
            workspace: "work"
        )
        XCTAssertNil(body["principal"])
        XCTAssertNil(body["workspace"])
        XCTAssertEqual(body["require_voice_prefix"] as? Bool, false)
    }
}
