import Foundation

/// iOS capture/playback preferences. Device-vs-Magician transport remains a
/// local mobile choice; backend provider/model selection is resolved by the
/// canonical Dictation profile whenever the Magician transport is used.
///
/// The two execution locations are **This iPhone** (Apple Speech + Voice) and
/// **Magician host** (the backend's configured STT/TTS via `/media/*`). The
/// persisted `cloud` raw value is retained for compatibility; the backend may
/// itself select a host-local or online provider.
enum STTSource: String, CaseIterable, Identifiable {
    case auto, onDevice = "on_device", cloud  // rawValue "cloud" == the Magician backend
    var id: String { rawValue }
    var label: String {
        switch self {
        case .auto: return "Auto (iPhone, backend fallback)"
        case .onDevice: return "This iPhone · Apple Speech"
        case .cloud: return "Backend host"
        }
    }
    /// True when we should try Apple's on-device recognizer first.
    var prefersOnDevice: Bool { self != .cloud }
    /// True when the Magician backend call is allowed (as primary or fallback).
    var allowsCloud: Bool { self != .onDevice }
}

enum TTSEngine: String, CaseIterable, Identifiable {
    case onDevice = "on_device", magician
    var id: String { rawValue }
    var label: String { self == .onDevice ? "This iPhone · Apple Voice" : "Backend host" }
}

/// Compatibility preference for explicit mode-specific `magican://voice?mode=…`
/// routes. Current widgets, system controls, installed mode-neutral shortcuts,
/// and bare `magican://voice` links use the single ambient `Talk to Magican` action and
/// `AmbientVoiceMode` below. The stored wire values remain decodable; they are no
/// longer presented as a competing launcher.
enum SystemVoiceLaunchMode: String, CaseIterable, Identifiable {
    case dictation
    case handsFree = "hands_free"
    case realtime

    var id: String { rawValue }

    var label: String {
        switch self {
        case .dictation: return "Dictate"
        case .handsFree: return "Hands-free"
        case .realtime: return "Live"
        }
    }

    var icon: String {
        switch self {
        case .dictation: return "mic.fill"
        case .handsFree: return "waveform"
        case .realtime: return "dot.radiowaves.left.and.right"
        }
    }

    var detail: String {
        switch self {
        case .dictation: return "Capture one message, then review or auto-send it."
        case .handsFree: return "Start the configured conversational audio pipeline."
        case .realtime: return "Start the selected low-latency realtime model."
        }
    }
}

/// The conversation pipeline used after an Ambient Listening wake.
///
/// This is deliberately broader than `VoiceEngine`: Hands-free and Live are
/// streaming voice transports, while Dictation is a turn-based recording → STT
/// → agent → TTS loop owned by the ambient call adapter. Keeping the types
/// separate makes it impossible to accidentally send `dictation` to a realtime
/// provider that only understands `hands_free` or `realtime`.
enum AmbientVoiceMode: String, CaseIterable, Identifiable {
    case dictation
    case handsFree = "hands_free"
    case realtime

    var id: String { rawValue }

    var label: String {
        switch self {
        case .dictation: return "Dictation"
        case .handsFree: return "Hands-free"
        case .realtime: return "Live"
        }
    }

    var icon: String {
        switch self {
        case .dictation: return "mic.fill"
        case .handsFree: return "waveform"
        case .realtime: return "dot.radiowaves.left.and.right"
        }
    }

    /// The streaming provider family, or nil for the native turn-based path.
    var streamingEngine: VoiceEngine? {
        switch self {
        case .dictation: return nil
        case .handsFree: return .handsFree
        case .realtime: return .realtime
        }
    }
}

/// How long an armed ambient window may listen before it disarms itself.
///
/// The leash on a microphone nobody is watching, and the user picks it. Every
/// option is finite, including the one whose label is not, and that is a
/// property of the architecture rather than a compromise: `AmbientArm` is stored
/// with a `capSeconds`, the orb is published with a `staleDate` at the expiry,
/// and that stale date is the only backstop that survives the app being killed
/// past its cap. An unbounded window would have no end the system could enforce.
enum AmbientLeash: String, CaseIterable, Identifiable {
    case thirtyMinutes = "30m"
    case twoHours = "2h"
    case untilStopped = "until_stopped"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .thirtyMinutes: return "30 min"
        case .twoHours: return "2 hours"
        case .untilStopped: return "Until I stop"
        }
    }

    /// The hard cap in seconds.
    ///
    /// `untilStopped` is **8 hours, and the number is derived rather than
    /// chosen**: ActivityKit ends a Live Activity of its own accord after eight
    /// hours. Past that point the orb is gone, and the orb is the disarm control
    /// — so any longer cap would guarantee a stretch of armed microphone with no
    /// way to stop it from outside the app, which is precisely the outcome this
    /// feature refuses everywhere else.
    var capSeconds: Int {
        switch self {
        case .thirtyMinutes: return 1_800
        case .twoHours: return 7_200
        case .untilStopped: return Int(AmbientExtensionPolicy.maximumWindowSeconds)
        }
    }

    var detail: String {
        switch self {
        case .thirtyMinutes: return "Magican stops listening after 30 minutes."
        case .twoHours: return "Magican stops listening after 2 hours."
        case .untilStopped:
            return "Magican keeps listening until you tap Stop listening — and stops after 8 hours regardless, because the Orb control cannot outlive that."
        }
    }
}

enum NativeAudioSurface: String, CaseIterable, Hashable {
    case dictation
    case handsFree = "hands_free"

    var label: String { self == .dictation ? "Dictation" : "Hands-free" }
}

enum NativeAudioStage: String, CaseIterable, Hashable {
    case vad
    case recordingSTT = "recording_stt"
    case streamingSTT = "streaming_stt"
    case diarization
    case tts

    var label: String {
        switch self {
        case .vad: return "Voice activity"
        case .recordingSTT: return "Transcription"
        case .streamingSTT: return "Live transcription"
        case .diarization: return "Speakers"
        case .tts: return "Voice"
        }
    }
}

struct NativeAudioStageOption: Identifiable, Equatable {
    let id: String
    let stage: NativeAudioStage
    let providerID: String
    let engineID: String
    let modelID: String
    let label: String
    let available: Bool
    let unavailableReason: String?

    /// Provider options in this catalog execute behind the Magician API. Make
    /// that boundary explicit on iOS so a macOS-local provider is not mistaken
    /// for an engine running on the phone itself.
    var displayLabel: String {
        let location: String
        switch engineID.lowercased() {
        case "macos_system", "fluid_audio": location = "Mac host"
        case "online": location = "Online"
        default: location = "Backend host"
        }
        return "\(location) · \(label)"
    }
}

struct NativeAudioProfileOption: Identifiable, Equatable {
    let id: String
    let surface: NativeAudioSurface
    let enabledStages: Set<NativeAudioStage>
    let providersByStage: [NativeAudioStage: [String]]

    var label: String {
        id.replacingOccurrences(of: "compat-", with: "")
            .replacingOccurrences(of: "-v[0-9]+$", with: "", options: .regularExpression)
            .replacingOccurrences(of: "-", with: " ")
            .split(separator: " ")
            .map { $0.prefix(1).uppercased() + $0.dropFirst() }
            .joined(separator: " ")
    }
}

struct NativeAudioCatalog: Equatable {
    let profiles: [NativeAudioProfileOption]
    let defaultProfiles: [NativeAudioSurface: String]
    let stageOptions: [NativeAudioStage: [NativeAudioStageOption]]
}

struct NativeAudioPreferenceSeed: Equatable {
    let profiles: [NativeAudioSurface: String]
    let stageOptions: [NativeAudioSurface: [NativeAudioStage: String]]

    static let empty = NativeAudioPreferenceSeed(profiles: [:], stageOptions: [:])
}

struct RealtimeVoiceProfileOption: Identifiable, Equatable {
    let id: String
    let label: String
    let provider: String
    let model: String
    let topology: String
    let mode: String
    let turnDetectionMode: String?
    let transcriptionModel: String?
    let transcriptionFallbackModel: String?
    let available: Bool
    let unavailableReason: String?

    init(
        id: String,
        label: String,
        provider: String,
        model: String,
        topology: String,
        mode: String,
        turnDetectionMode: String?,
        transcriptionModel: String? = nil,
        transcriptionFallbackModel: String? = nil,
        available: Bool,
        unavailableReason: String?
    ) {
        self.id = id
        self.label = label
        self.provider = provider
        self.model = model
        self.topology = topology
        self.mode = mode
        self.turnDetectionMode = turnDetectionMode
        self.transcriptionModel = transcriptionModel
        self.transcriptionFallbackModel = transcriptionFallbackModel
        self.available = available
        self.unavailableReason = unavailableReason
    }

    var isTranslation: Bool { mode == "translation" }
    var isSupportedByNativeClient: Bool { topology == "backend_proxied" }
    var defaultsToPushToTalk: Bool {
        !isTranslation && turnDetectionMode?.lowercased() == "none"
    }
    var transcriptionLabel: String? {
        guard let model = transcriptionModel?.trimmingCharacters(in: .whitespacesAndNewlines),
              !model.isEmpty else { return nil }
        if model.caseInsensitiveCompare("local") == .orderedSame {
            return "Parallel STT · Backend host"
        }
        return "Transcription · \(model)"
    }
}

struct RealtimeVoiceCatalog: Equatable {
    let profiles: [RealtimeVoiceProfileOption]
    let defaultProfileID: String?
}

final class AudioSettings: ObservableObject {
    static let shared = AudioSettings()
    static let archiveChatDictationDefault = false

    private let store = MagicianAccess.store
    private let principal = MagicianAccess.principal
    private let workspace = MagicianAccess.workspace

    @Published var sttSource: STTSource {
        didSet { store.set(sttSource.rawValue, forKey: Keys.sttSource) }
    }
    @Published var ttsEngine: TTSEngine {
        didSet { store.set(ttsEngine.rawValue, forKey: Keys.ttsEngine) }
    }
    /// Speak assistant replies aloud (auto-speak).
    @Published var speakReplies: Bool {
        didSet { store.set(speakReplies, forKey: Keys.speakReplies) }
    }
    /// Explicit, device-local consent to retain Chat dictation recordings as
    /// durable Audio Notes. It is deliberately off on fresh installs: using the
    /// microphone to compose text must not silently create a permanent raw-audio
    /// archive.
    @Published var archiveChatDictation: Bool {
        didSet { store.set(archiveChatDictation, forKey: Keys.archiveChatDictation) }
    }
    /// Compatibility mode used by explicit mode-specific voice deep links.
    /// Public system Talk surfaces use `ambientVoiceMode` below.
    @Published var systemVoiceLaunchMode: SystemVoiceLaunchMode {
        didSet { store.set(systemVoiceLaunchMode.rawValue, forKey: Keys.systemVoiceLaunchMode) }
    }
    /// Ignore open-mic room speech unless it starts with the primary agent's
    /// negotiated "Hey <name or alias>" phrase.
    @Published var requireVoicePrefix: Bool {
        didSet {
            store.set(requireVoicePrefix, forKey: Keys.requireVoicePrefix)
            if !applyingBackendPreferences { enqueueVoicePrefixPersistence() }
        }
    }
    /// How long an armed ambient window runs before it disarms itself.
    /// Device-local, like every other capture preference here — an armed window
    /// is a tap on *this* phone's microphone and cannot mean anything elsewhere.
    @Published var ambientLeash: AmbientLeash {
        didSet { store.set(ambientLeash.rawValue, forKey: Keys.ambientLeash) }
    }
    /// Conversation engine used only after an Ambient Listening wake. It is
    /// seeded once from backend media preferences and then remains local to this
    /// iPhone, independently of in-app chat and system voice launch choices.
    @Published var ambientVoiceMode: AmbientVoiceMode {
        didSet { store.set(ambientVoiceMode.rawValue, forKey: Keys.ambientVoiceMode) }
    }
    /// Which engine a Live call opens with. These Live-call controls are
    /// device-local: changing iOS never changes Web or backend preferences.
    @Published var liveVoiceEngine: VoiceEngine {
        didSet { store.set(liveVoiceEngine.rawValue, forKey: Keys.liveVoiceEngine) }
    }
    /// Native Live-call provider profile. The existing backend-proxied GPT mini
    /// remains the device default; selecting Gemini is an explicit local choice.
    @Published var realtimeVoiceProfile: String {
        didSet { store.set(realtimeVoiceProfile, forKey: Keys.realtimeVoiceProfile) }
    }
    /// Per-device default turn boundary for the next iOS Live call.
    @Published var liveVoicePttOn: Bool {
        didSet { store.set(liveVoicePttOn, forKey: Keys.liveVoicePttOn) }
    }
    @Published private(set) var realtimeVoiceProfiles: [RealtimeVoiceProfileOption] = []
    @Published private(set) var nativeAudioProfiles: [NativeAudioProfileOption] = []
    @Published private(set) var nativeAudioStageOptions: [NativeAudioStage: [NativeAudioStageOption]] = [:]
    @Published private(set) var selectedNativeProfiles: [NativeAudioSurface: String] {
        didSet { persistNativeSelections() }
    }
    @Published private(set) var selectedNativeStageOptions: [NativeAudioSurface: [NativeAudioStage: String]] {
        didSet { persistNativeSelections() }
    }

    private var applyingBackendPreferences = false
    private var confirmedVoicePrefix = true
    private var pendingVoicePrefix: Bool?
    private var voicePrefixWriteInFlight = false
    private var pendingNativeCatalog: NativeAudioCatalog?
    private var pendingNativePreferenceSeed: NativeAudioPreferenceSeed = .empty
    private var mediaPreferenceSeedCompleted = false
    private var mediaPreferenceSeedSucceeded = false
    private var providerCatalogSeedCompleted = false

    private enum Keys {
        static let sttSource = "audio_stt_source"
        static let ttsEngine = "audio_tts_engine"
        static let speakReplies = "audio_speak_replies"
        static let archiveChatDictation = "audio_archive_chat_dictation"
        static let systemVoiceLaunchMode = "audio_system_voice_launch_mode"
        static let requireVoicePrefix = "audio_require_voice_prefix"
        static let ambientLeash = "audio_ambient_leash"
        // Keep the shipped key while widening its value set to include
        // `dictation`; existing Hands-free/Live choices migrate without work.
        static let ambientVoiceMode = "audio_ambient_voice_engine"
        static let liveVoiceEngine = "audio_live_voice_engine"
        static let realtimeVoiceProfile = "audio_realtime_voice_profile"
        static let liveVoicePttOn = "audio_live_voice_ptt_on"
        static let seeded = "audio_settings_seeded"
        static let nativeProfiles = "audio_native_surface_profiles"
        static let nativeStageOptions = "audio_native_surface_stage_options"
        static let nativeSurfacesSeeded = "audio_native_surfaces_seeded"
    }

    private init() {
        sttSource = STTSource(rawValue: store.string(forKey: Keys.sttSource) ?? "") ?? .auto
        ttsEngine = TTSEngine(rawValue: store.string(forKey: Keys.ttsEngine) ?? "") ?? .onDevice
        speakReplies = store.bool(forKey: Keys.speakReplies)
        archiveChatDictation = (store.object(forKey: Keys.archiveChatDictation) as? Bool)
            ?? Self.archiveChatDictationDefault
        systemVoiceLaunchMode = SystemVoiceLaunchMode(
            rawValue: store.string(forKey: Keys.systemVoiceLaunchMode) ?? ""
        ) ?? .dictation
        requireVoicePrefix = (store.object(forKey: Keys.requireVoicePrefix) as? Bool) ?? true
        // Defaults to the design's canonical long leash rather than the shortest
        // option: the window is explicitly armed by the user each time and its
        // countdown is on the orb throughout, so the risk a shorter default
        // guards against is one the user has already been shown.
        ambientLeash = AmbientLeash(rawValue: store.string(forKey: Keys.ambientLeash) ?? "") ?? .twoHours
        ambientVoiceMode = AmbientVoiceMode(
            rawValue: store.string(forKey: Keys.ambientVoiceMode) ?? ""
        ) ?? .handsFree
        liveVoiceEngine = VoiceEngine(rawValue: store.string(forKey: Keys.liveVoiceEngine) ?? "")
            ?? .realtime
        realtimeVoiceProfile = store.string(forKey: Keys.realtimeVoiceProfile) ?? ""
        liveVoicePttOn = (store.object(forKey: Keys.liveVoicePttOn) as? Bool) ?? false
        selectedNativeProfiles = Self.loadNativeProfiles(from: store, key: Keys.nativeProfiles)
        selectedNativeStageOptions = Self.loadNativeStageOptions(
            from: store,
            key: Keys.nativeStageOptions
        )
        confirmedVoicePrefix = requireVoicePrefix
    }

    func updateRealtimeVoiceProfiles(_ catalog: RealtimeVoiceCatalog) {
        let storedSelection = store.object(forKey: Keys.realtimeVoiceProfile) != nil
        let requestedSelection = storedSelection ? realtimeVoiceProfile : nil
        let nativeProfiles = Self.orderedNativeRealtimeProfiles(
            from: catalog.profiles,
            selectedID: requestedSelection,
            backendDefaultID: catalog.defaultProfileID
        )
        realtimeVoiceProfiles = nativeProfiles
        if let resolved = Self.resolveRealtimeVoiceProfile(
            from: catalog.profiles,
            selectedID: requestedSelection,
            backendDefaultID: catalog.defaultProfileID
        ), resolved != realtimeVoiceProfile {
            realtimeVoiceProfile = resolved
        }
        if store.object(forKey: Keys.liveVoicePttOn) == nil,
           let selected = nativeProfiles.first(where: { $0.id == realtimeVoiceProfile }) {
            liveVoicePttOn = Self.resolveLiveVoicePttOn(
                requested: selected.defaultsToPushToTalk,
                engine: .realtime,
                profile: selected
            )
        }
    }

    /// Keep the active device choice prominent while retaining the backend's
    /// default native equivalent immediately after it. The backend default can
    /// be a browser-only direct WebRTC profile; `resolveRealtimeVoiceProfile`
    /// maps that profile to its backend-proxied model/provider equivalent for
    /// native clients before ordering.
    static func orderedNativeRealtimeProfiles(
        from profiles: [RealtimeVoiceProfileOption],
        selectedID: String?,
        backendDefaultID: String?
    ) -> [RealtimeVoiceProfileOption] {
        let nativeProfiles = profiles.filter(\.isSupportedByNativeClient)
        let resolvedSelection = resolveRealtimeVoiceProfile(
            from: profiles,
            selectedID: selectedID,
            backendDefaultID: backendDefaultID
        )
        let resolvedDefault = resolveRealtimeVoiceProfile(
            from: profiles,
            selectedID: nil,
            backendDefaultID: backendDefaultID
        )

        func rank(_ profile: RealtimeVoiceProfileOption) -> Int {
            if profile.id == resolvedSelection { return 0 }
            if profile.id == resolvedDefault { return 1 }
            return profile.available ? 2 : 3
        }

        return nativeProfiles.enumerated()
            .sorted { left, right in
                let leftRank = rank(left.element)
                let rightRank = rank(right.element)
                return leftRank == rightRank ? left.offset < right.offset : leftRank < rightRank
            }
            .map(\.element)
    }

    static func resolveRealtimeVoiceProfile(
        from profiles: [RealtimeVoiceProfileOption],
        selectedID: String?,
        backendDefaultID: String?
    ) -> String? {
        let nativeProfiles = profiles.filter {
            $0.isSupportedByNativeClient && $0.available
        }
        if let selectedID,
           nativeProfiles.contains(where: { $0.id == selectedID }) {
            return selectedID
        }
        if let backendDefaultID,
           nativeProfiles.contains(where: { $0.id == backendDefaultID }) {
            return backendDefaultID
        }
        if let backendDefault = profiles.first(where: { $0.id == backendDefaultID }) {
            let family = providerFamily(backendDefault.provider)
            if let equivalent = nativeProfiles.first(where: {
                $0.model == backendDefault.model && providerFamily($0.provider) == family
            }) {
                return equivalent.id
            }
            if let sameFamily = nativeProfiles.first(where: {
                providerFamily($0.provider) == family
            }) {
                return sameFamily.id
            }
        }
        return nativeProfiles.first?.id
    }

    private static func providerFamily(_ provider: String) -> String {
        provider.lowercased().replacingOccurrences(of: "_backend", with: "")
    }

    static func resolveLiveVoicePttOn(
        requested: Bool,
        engine: VoiceEngine,
        profile: RealtimeVoiceProfileOption?
    ) -> Bool {
        engine == .realtime && profile?.isTranslation == true ? false : requested
    }

    /// Seed the independent iPhone preference from the shared backend value.
    /// Recording is preserved as a complete turn-based ambient conversation,
    /// rather than silently widened into Hands-free.
    static func ambientVoiceModeSeed(backendVoiceMode: String?) -> AmbientVoiceMode {
        switch backendVoiceMode?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
        case VoiceEngine.realtime.voiceMode, "live": return .realtime
        case VoiceEngine.handsFree.voiceMode, "handsfree": return .handsFree
        case "recording", "dictation", "dictate": return .dictation
        default: return .handsFree
        }
    }

    /// Apply a native-compatible profile selection without touching shared
    /// backend preferences. Translation profiles force continuous capture.
    @discardableResult
    func selectRealtimeVoiceProfile(_ profile: RealtimeVoiceProfileOption) -> Bool {
        guard profile.available, profile.isSupportedByNativeClient else { return false }
        let changed = realtimeVoiceProfile != profile.id
        realtimeVoiceProfile = profile.id
        if liveVoiceEngine == .realtime && profile.isTranslation {
            liveVoicePttOn = false
        }
        return changed
    }

    func profiles(for surface: NativeAudioSurface) -> [NativeAudioProfileOption] {
        nativeAudioProfiles
            .filter { $0.surface == surface }
            .sorted { $0.label.localizedCaseInsensitiveCompare($1.label) == .orderedAscending }
    }

    func selectedProfile(for surface: NativeAudioSurface) -> NativeAudioProfileOption? {
        guard let id = selectedNativeProfiles[surface] else { return nil }
        return nativeAudioProfiles.first { $0.id == id && $0.surface == surface }
    }

    func stageOptions(
        for stage: NativeAudioStage,
        profile: NativeAudioProfileOption
    ) -> [NativeAudioStageOption] {
        let configured = profile.providersByStage[stage] ?? []
        let options = nativeAudioStageOptions[stage] ?? []
        return configured.compactMap { providerID in
            options.first { $0.providerID == providerID || $0.id == providerID }
        }
    }

    func isNativeProfileAvailable(_ profile: NativeAudioProfileOption) -> Bool {
        guard profile.surface == .handsFree else { return true }
        return Self.handsFreeProfileAvailable(
            profile,
            stageOptions: nativeAudioStageOptions
        )
    }

    /// `nil` means the catalog has not loaded, so callers can retain the
    /// backend's coarse availability fallback instead of disabling the UI.
    func selectedNativeProfileAvailable(for surface: NativeAudioSurface) -> Bool? {
        guard let profile = selectedProfile(for: surface) else { return nil }
        return isNativeProfileAvailable(profile)
    }

    /// Whether the loaded catalog contains any usable profile for this surface.
    /// `nil` distinguishes a catalog that has not loaded from a loaded catalog
    /// with no usable profiles.
    func hasAvailableNativeProfile(for surface: NativeAudioSurface) -> Bool? {
        guard pendingNativeCatalog != nil else { return nil }
        return Self.hasAvailableNativeProfile(
            for: surface,
            profiles: nativeAudioProfiles,
            stageOptions: nativeAudioStageOptions
        )
    }

    static func hasAvailableNativeProfile(
        for surface: NativeAudioSurface,
        profiles: [NativeAudioProfileOption],
        stageOptions: [NativeAudioStage: [NativeAudioStageOption]]
    ) -> Bool {
        profiles.lazy
            .filter { $0.surface == surface }
            .contains { profile in
                surface != .handsFree
                    || handsFreeProfileAvailable(profile, stageOptions: stageOptions)
            }
    }

    static func handsFreeProfileAvailable(
        _ profile: NativeAudioProfileOption,
        stageOptions: [NativeAudioStage: [NativeAudioStageOption]]
    ) -> Bool {
        guard profile.surface == .handsFree else { return false }
        let requiredStages: [NativeAudioStage] = [.vad, .streamingSTT, .tts]
        return requiredStages.allSatisfy { stage in
            guard profile.enabledStages.contains(stage) else { return false }
            let configured = profile.providersByStage[stage] ?? []
            return stageOptions[stage, default: []].contains { option in
                option.available && configured.contains { candidate in
                    candidate.caseInsensitiveCompare(option.providerID) == .orderedSame
                        || candidate.caseInsensitiveCompare(option.id) == .orderedSame
                }
            }
        }
    }

    func selectNativeProfile(_ profile: NativeAudioProfileOption) {
        selectedNativeProfiles[profile.surface] = profile.id
        selectedNativeStageOptions[profile.surface] = [:]
    }

    func selectNativeStageOption(
        _ optionID: String?,
        stage: NativeAudioStage,
        surface: NativeAudioSurface
    ) {
        var stageSelections = selectedNativeStageOptions[surface] ?? [:]
        if let optionID, !optionID.isEmpty {
            stageSelections[stage] = optionID
        } else {
            stageSelections.removeValue(forKey: stage)
        }
        selectedNativeStageOptions[surface] = stageSelections
    }

    func requestProfile(for surface: NativeAudioSurface) -> String? {
        selectedNativeProfiles[surface]
    }

    func requestStageOptions(for surface: NativeAudioSurface) -> [String: String] {
        Dictionary(uniqueKeysWithValues: (selectedNativeStageOptions[surface] ?? [:]).map {
            ($0.key.rawValue, $0.value)
        })
    }

    /// Adopt backend-owned cross-surface preferences. Auto-speak is seeded once
    /// to preserve an explicit local choice; the voice address gate is refreshed
    /// on launch for legacy callers that do not send a per-session override.
    /// Surface profiles/stages are seeded once, then remain local to this
    /// device. Native requests carry them as immutable request/session
    /// overrides, so changing iOS does not rewrite Web preferences.
    func seedFromBackendIfNeeded() {
        seedMediaPreferencesFromBackend()
        seedRealtimeCatalogFromBackend()
    }

    private func seedMediaPreferencesFromBackend() {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/media/preferences") else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        URLSession.shared.dataTask(with: request) { [weak self] data, response, _ in
            let obj = Self.successfulMediaPreferencesJSONObject(data: data, response: response)
            DispatchQueue.main.async {
                guard let self else { return }
                if let obj {
                    let autoSpeak = obj["auto_speak"] as? Bool ?? false
                    let requireVoicePrefix = obj["require_voice_prefix"] as? Bool ?? true
                    let voiceMode = obj["voice_mode"] as? String
                    if self.store.object(forKey: Keys.ambientVoiceMode) == nil {
                        self.ambientVoiceMode = Self.ambientVoiceModeSeed(
                            backendVoiceMode: voiceMode
                        )
                    }
                    if self.store.object(forKey: Keys.liveVoiceEngine) == nil {
                        self.liveVoiceEngine = voiceMode == "hands_free" ? .handsFree : .realtime
                    }
                    if !self.voicePrefixWriteInFlight && self.pendingVoicePrefix == nil {
                        self.applyConfirmedVoicePrefix(requireVoicePrefix)
                    }
                    if !self.store.bool(forKey: Keys.seeded) {
                        self.speakReplies = autoSpeak
                        self.store.set(true, forKey: Keys.seeded)
                    }
                }
                self.pendingNativePreferenceSeed = Self.nativePreferenceSeed(from: obj ?? [:])
                self.mediaPreferenceSeedCompleted = true
                self.mediaPreferenceSeedSucceeded = obj != nil
                self.finishNativeSurfaceSeedIfReady()
            }
        }.resume()
    }

    private func seedRealtimeCatalogFromBackend() {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/media/providers") else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        URLSession.shared.dataTask(with: request) { [weak self] data, response, _ in
            let object = Self.successfulProviderCatalogJSONObject(data: data, response: response)
            DispatchQueue.main.async {
                guard let self else { return }
                if let object {
                    self.updateRealtimeVoiceProfiles(Self.realtimeVoiceCatalog(from: object))
                    self.pendingNativeCatalog = Self.nativeAudioCatalog(from: object)
                }
                self.providerCatalogSeedCompleted = true
                self.finishNativeSurfaceSeedIfReady()
            }
        }.resume()
    }

    static func successfulJSONObject(data: Data?, response: URLResponse?) -> [String: Any]? {
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode),
              let data else { return nil }
        return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    }

    static func successfulMediaPreferencesJSONObject(
        data: Data?,
        response: URLResponse?
    ) -> [String: Any]? {
        guard let object = successfulJSONObject(data: data, response: response),
              validMediaPreferencesPayload(object) else { return nil }
        return object
    }

    static func successfulProviderCatalogJSONObject(
        data: Data?,
        response: URLResponse?
    ) -> [String: Any]? {
        guard let object = successfulJSONObject(data: data, response: response),
              validProviderCatalogPayload(object) else { return nil }
        return object
    }

    private static func validMediaPreferencesPayload(_ object: [String: Any]) -> Bool {
        guard let schemaVersion = object["schema_version"] as? Int,
              schemaVersion > 0,
              object["auto_speak"] is Bool,
              let voiceMode = object["voice_mode"] as? String,
              !voiceMode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              object["require_voice_prefix"] is Bool else { return false }
        if let profiles = object["surface_profiles"], !isStringMap(profiles) {
            return false
        }
        if let stageOptions = object["surface_stage_options"],
           !isNestedStringMap(stageOptions) {
            return false
        }
        return true
    }

    private static func validProviderCatalogPayload(_ object: [String: Any]) -> Bool {
        guard let revision = object["audio_revision"] as? String,
              !revision.isEmpty,
              let stages = object["stages"] as? [String: Any],
              let surfaceProfiles = object["surface_profiles"] as? [String: Any],
              let defaultProfiles = object["default_surface_profiles"],
              isStringMap(defaultProfiles),
              object["engines"] is [String: Any],
              object["hands_free_voice"] is Bool else { return false }
        guard stages.values.allSatisfy(validStageOptions),
              surfaceProfiles.values.allSatisfy(validSurfaceProfile) else { return false }
        if let profiles = object["realtime_voice_profiles"],
           let profiles = profiles as? [[String: Any]] {
            return profiles.allSatisfy(validRealtimeVoiceProfile)
        } else if object["realtime_voice_profiles"] != nil {
            return false
        }
        return true
    }

    private static func validStageOptions(_ value: Any) -> Bool {
        guard let options = value as? [[String: Any]] else { return false }
        return options.allSatisfy { option in
            option["option_id"] is String
                && option["provider_id"] is String
                && option["engine_id"] is String
                && option["model_id"] is String
                && option["label"] is String
                && option["availability"] is String
        }
    }

    private static func validSurfaceProfile(_ value: Any) -> Bool {
        guard let profile = value as? [String: Any], profile["surface"] is String else {
            return false
        }
        return NativeAudioStage.allCases.allSatisfy { stage in
            guard let config = profile[stage.rawValue] as? [String: Any],
                  let enabled = config["enabled"] as? Bool else { return false }
            if let providers = config["providers"] {
                return providers is [String]
            }
            return !enabled
        }
    }

    private static func validRealtimeVoiceProfile(_ profile: [String: Any]) -> Bool {
        profile["profile_id"] is String
            && profile["label"] is String
            && profile["provider"] is String
            && profile["model"] is String
            && profile["topology"] is String
            && profile["mode"] is String
            && (profile["transcription_model"] == nil
                || profile["transcription_model"] is String)
            && (profile["transcription_fallback_model"] == nil
                || profile["transcription_fallback_model"] is String)
            && profile["available"] is Bool
    }

    private static func isStringMap(_ value: Any) -> Bool {
        guard let dictionary = value as? [String: Any] else { return false }
        return dictionary.values.allSatisfy { $0 is String }
    }

    private static func isNestedStringMap(_ value: Any) -> Bool {
        guard let dictionary = value as? [String: Any] else { return false }
        return dictionary.values.allSatisfy(isStringMap)
    }

    static func realtimeVoiceCatalog(from object: [String: Any]) -> RealtimeVoiceCatalog {
        let profiles = (object["realtime_voice_profiles"] as? [[String: Any]] ?? [])
            .compactMap { raw -> RealtimeVoiceProfileOption? in
                guard let id = raw["profile_id"] as? String,
                      let label = raw["label"] as? String,
                      let provider = raw["provider"] as? String,
                      let model = raw["model"] as? String,
                      let topology = raw["topology"] as? String,
                      let mode = raw["mode"] as? String else { return nil }
                return RealtimeVoiceProfileOption(
                    id: id,
                    label: label,
                    provider: provider,
                    model: model,
                    topology: topology,
                    mode: mode,
                    turnDetectionMode: raw["turn_detection_mode"] as? String,
                    transcriptionModel: raw["transcription_model"] as? String,
                    transcriptionFallbackModel: raw["transcription_fallback_model"] as? String,
                    available: raw["available"] as? Bool ?? false,
                    unavailableReason: raw["unavailable_reason"] as? String
                )
            }
        return RealtimeVoiceCatalog(
            profiles: profiles,
            defaultProfileID: object["realtime_voice_default_profile"] as? String
        )
    }

    static func nativeAudioCatalog(from object: [String: Any]) -> NativeAudioCatalog {
        let profilesObject = object["surface_profiles"] as? [String: Any] ?? [:]
        let profiles = profilesObject.compactMap { id, value -> NativeAudioProfileOption? in
            guard let raw = value as? [String: Any],
                  let surfaceValue = raw["surface"] as? String,
                  let surface = NativeAudioSurface(rawValue: surfaceValue) else { return nil }
            var enabledStages = Set<NativeAudioStage>()
            var providersByStage: [NativeAudioStage: [String]] = [:]
            for stage in NativeAudioStage.allCases {
                guard let stageRaw = raw[stage.rawValue] as? [String: Any],
                      stageRaw["enabled"] as? Bool == true else { continue }
                enabledStages.insert(stage)
                providersByStage[stage] = stageRaw["providers"] as? [String] ?? []
            }
            return NativeAudioProfileOption(
                id: id,
                surface: surface,
                enabledStages: enabledStages,
                providersByStage: providersByStage
            )
        }

        let defaultsObject = object["default_surface_profiles"] as? [String: Any] ?? [:]
        let defaults = Dictionary(uniqueKeysWithValues: NativeAudioSurface.allCases.compactMap {
            surface -> (NativeAudioSurface, String)? in
            guard let id = defaultsObject[surface.rawValue] as? String else { return nil }
            return (surface, id)
        })

        let stagesObject = object["stages"] as? [String: Any] ?? [:]
        var stageOptions: [NativeAudioStage: [NativeAudioStageOption]] = [:]
        for stage in NativeAudioStage.allCases {
            let rawOptions = stagesObject[stage.rawValue] as? [[String: Any]] ?? []
            stageOptions[stage] = rawOptions.compactMap { raw in
                guard let id = raw["option_id"] as? String,
                      let providerID = raw["provider_id"] as? String,
                      let engineID = raw["engine_id"] as? String,
                      let modelID = raw["model_id"] as? String,
                      let label = raw["label"] as? String else { return nil }
                return NativeAudioStageOption(
                    id: id,
                    stage: stage,
                    providerID: providerID,
                    engineID: engineID,
                    modelID: modelID,
                    label: label,
                    available: (raw["availability"] as? String) == "available",
                    unavailableReason: raw["unavailable_reason"] as? String
                )
            }
        }
        return NativeAudioCatalog(
            profiles: profiles,
            defaultProfiles: defaults,
            stageOptions: stageOptions
        )
    }

    static func nativePreferenceSeed(from object: [String: Any]) -> NativeAudioPreferenceSeed {
        let profileObject = object["surface_profiles"] as? [String: Any] ?? [:]
        let profiles = Dictionary(uniqueKeysWithValues: NativeAudioSurface.allCases.compactMap {
            surface -> (NativeAudioSurface, String)? in
            guard let id = profileObject[surface.rawValue] as? String else { return nil }
            return (surface, id)
        })
        let stagesObject = object["surface_stage_options"] as? [String: Any] ?? [:]
        var stageOptions: [NativeAudioSurface: [NativeAudioStage: String]] = [:]
        for surface in NativeAudioSurface.allCases {
            guard let raw = stagesObject[surface.rawValue] as? [String: Any] else { continue }
            stageOptions[surface] = Dictionary(uniqueKeysWithValues: NativeAudioStage.allCases.compactMap {
                stage -> (NativeAudioStage, String)? in
                guard let id = raw[stage.rawValue] as? String else { return nil }
                return (stage, id)
            })
        }
        return NativeAudioPreferenceSeed(profiles: profiles, stageOptions: stageOptions)
    }

    static func retainedStageSelections(
        _ requested: [NativeAudioStage: String],
        profile: NativeAudioProfileOption,
        stageOptions: [NativeAudioStage: [NativeAudioStageOption]]
    ) -> [NativeAudioStage: String] {
        requested.filter { stage, optionID in
            guard profile.enabledStages.contains(stage) else { return false }
            let configured = profile.providersByStage[stage] ?? []
            return stageOptions[stage, default: []].contains { option in
                option.id == optionID && configured.contains { candidate in
                    candidate.caseInsensitiveCompare(option.providerID) == .orderedSame
                        || candidate.caseInsensitiveCompare(option.id) == .orderedSame
                }
            }
        }
    }

    private func finishNativeSurfaceSeedIfReady() {
        guard mediaPreferenceSeedCompleted, providerCatalogSeedCompleted,
              let catalog = pendingNativeCatalog else { return }
        nativeAudioProfiles = catalog.profiles
        nativeAudioStageOptions = catalog.stageOptions

        var profiles = selectedNativeProfiles
        var stages = selectedNativeStageOptions
        let firstSeed = !store.bool(forKey: Keys.nativeSurfacesSeeded)
        guard !firstSeed || mediaPreferenceSeedSucceeded else { return }
        for surface in NativeAudioSurface.allCases {
            let candidates = catalog.profiles.filter { $0.surface == surface }
            let requested = firstSeed ? pendingNativePreferenceSeed.profiles[surface] : profiles[surface]
            let fallback = catalog.defaultProfiles[surface]
            let selected = [requested, fallback]
                .compactMap { $0 }
                .first { id in candidates.contains { $0.id == id } }
                ?? candidates.first?.id
            guard let selected else {
                profiles.removeValue(forKey: surface)
                stages.removeValue(forKey: surface)
                continue
            }
            profiles[surface] = selected

            let requestedStages = firstSeed
                ? pendingNativePreferenceSeed.stageOptions[surface] ?? [:]
                : stages[surface] ?? [:]
            guard let activeProfile = candidates.first(where: { $0.id == selected }) else {
                stages.removeValue(forKey: surface)
                continue
            }
            stages[surface] = Self.retainedStageSelections(
                requestedStages,
                profile: activeProfile,
                stageOptions: catalog.stageOptions
            )
        }
        selectedNativeProfiles = profiles
        selectedNativeStageOptions = stages
        if !firstSeed || mediaPreferenceSeedSucceeded {
            store.set(true, forKey: Keys.nativeSurfacesSeeded)
        }
    }

    private func persistNativeSelections() {
        let profiles = Dictionary(uniqueKeysWithValues: selectedNativeProfiles.map {
            ($0.key.rawValue, $0.value)
        })
        let stages = Dictionary(uniqueKeysWithValues: selectedNativeStageOptions.map { surface, values in
            (surface.rawValue, Dictionary(uniqueKeysWithValues: values.map {
                ($0.key.rawValue, $0.value)
            }))
        })
        store.set(profiles, forKey: Keys.nativeProfiles)
        store.set(stages, forKey: Keys.nativeStageOptions)
    }

    private static func loadNativeProfiles(
        from store: UserDefaults,
        key: String
    ) -> [NativeAudioSurface: String] {
        let raw = store.dictionary(forKey: key) as? [String: String] ?? [:]
        return Dictionary(uniqueKeysWithValues: raw.compactMap { key, value in
            NativeAudioSurface(rawValue: key).map { ($0, value) }
        })
    }

    private static func loadNativeStageOptions(
        from store: UserDefaults,
        key: String
    ) -> [NativeAudioSurface: [NativeAudioStage: String]] {
        let raw = store.dictionary(forKey: key) ?? [:]
        var result: [NativeAudioSurface: [NativeAudioStage: String]] = [:]
        for (surfaceKey, value) in raw {
            guard let surface = NativeAudioSurface(rawValue: surfaceKey),
                  let values = value as? [String: String] else { continue }
            result[surface] = Dictionary(uniqueKeysWithValues: values.compactMap { key, value in
                NativeAudioStage(rawValue: key).map { ($0, value) }
            })
        }
        return result
    }

    static func voicePrefixPreferenceBody(
        required: Bool,
        principal _: String,
        workspace _: String
    ) -> [String: Any] {
        [
            "require_voice_prefix": required
        ]
    }

    private func enqueueVoicePrefixPersistence() {
        pendingVoicePrefix = requireVoicePrefix
        flushVoicePrefixPersistence()
    }

    /// Keep writes strictly ordered. A rapid second toggle waits for the first
    /// response, preventing an older request from arriving last and silently
    /// restoring stale backend state.
    private func flushVoicePrefixPersistence() {
        guard !voicePrefixWriteInFlight, let required = pendingVoicePrefix else { return }
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/media/preferences") else { return }
        pendingVoicePrefix = nil
        voicePrefixWriteInFlight = true
        var request = URLRequest(url: url)
        request.httpMethod = "PUT"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = try? JSONSerialization.data(withJSONObject: Self.voicePrefixPreferenceBody(
            required: required,
            principal: principal,
            workspace: workspace
        ))
        URLSession.shared.dataTask(with: request) { [weak self] data, response, _ in
            DispatchQueue.main.async {
                guard let self else { return }
                self.voicePrefixWriteInFlight = false
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                let object = data.flatMap {
                    try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
                }
                if (200..<300).contains(status) {
                    self.confirmedVoicePrefix = object?["require_voice_prefix"] as? Bool ?? required
                    if self.pendingVoicePrefix == nil {
                        self.applyConfirmedVoicePrefix(self.confirmedVoicePrefix)
                    }
                } else if self.pendingVoicePrefix == nil {
                    self.applyConfirmedVoicePrefix(self.confirmedVoicePrefix)
                }
                self.flushVoicePrefixPersistence()
            }
        }.resume()
    }

    private func applyConfirmedVoicePrefix(_ value: Bool) {
        confirmedVoicePrefix = value
        applyingBackendPreferences = true
        requireVoicePrefix = value
        applyingBackendPreferences = false
    }
}
