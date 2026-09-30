import Foundation

/// Which provider family serves a Live call.
///
/// INDEPENDENT of the turn boundary (see `startPayload`'s `pttOn`). Conflating
/// the two is what once made the default iOS Live call run on the cascaded
/// engine instead of GPT Realtime.
enum VoiceEngine: String, Equatable, Sendable, CaseIterable {
    /// Backend-proxied GPT Realtime session.
    case realtime
    /// Local cascaded pipeline: VAD → streaming STT → chat → TTS.
    case handsFree

    /// The backend `voice_mode` wire value.
    var voiceMode: String {
        switch self {
        case .realtime: return "realtime"
        case .handsFree: return "hands_free"
        }
    }

    /// Short label for the in-call engine control.
    var displayName: String {
        switch self {
        case .realtime: return "Realtime"
        case .handsFree: return "Hands-free"
        }
    }
}

/// Pure control-plane protocol for the Live voice call (mirrors web realtimeVoiceClient.ts).
/// Builds outgoing `{kind,payload}` envelopes and routes incoming server events to client
/// actions — no transport, so it's unit-testable.
enum RealtimeVoiceProtocol {
    struct Addressing: Equatable {
        let required: Bool
        let activationPhrases: [String]
        let followUpWindowMs: Int

        static let disabled = Addressing(required: false, activationPhrases: [], followUpWindowMs: 8_000)
    }

    enum Role: Equatable { case user, assistant }
    enum Event: Equatable {
        case ready, rotating, rebind, ended, ignore
        /// A server error. `recoverable` errors (e.g. a transient tool failure)
        /// must NOT tear the call down — mirror the web `!payload.recoverable` guard.
        case error(message: String, recoverable: Bool)
        case transcript(role: Role, text: String, itemID: String?, turnGeneration: Int?)
        case transcriptPartial(role: Role, text: String, itemID: String?, turnGeneration: Int?)
        case transcriptCleared(itemID: String?, turnGeneration: Int?, reason: String)
        case transcriptIgnored(itemID: String?, turnGeneration: Int?, reason: String)
        case toolResult
        /// Native iOS owns presentation for source-free Tutor. The backend has
        /// already interrupted its ordinary reply and deduplicated the turn.
        case tutorBlackboardRequested(text: String, quick: Bool)
        /// A transient visible notice accompanies the immediate spoken reply.
        case guidedFlowRejected(message: String, backendAnnounced: Bool)
        /// The server closed the reply's audio (`audio.output.ended`, or the
        /// cascaded engine's `response.interrupted`). `interrupted == true`
        /// means the reply was cut off mid-stream: bytes already streamed may
        /// still sit in this client's player queue, and the server stopped
        /// accounting for them at the interrupt — its self-echo window
        /// collapsed to that instant (`media_rails/self_echo.rs`,
        /// `note_assistant_audio_done` / `truncate_playback`). A client that
        /// keeps playing the orphaned remainder re-opens exactly the gap that
        /// collapse closed, so playback must be flushed. A natural end
        /// (`interrupted == false`) asks nothing — the queue drains truthfully.
        case assistantAudioEnded(interrupted: Bool)
        /// `interaction.status`: the assistant finished an utterance but is
        /// still on the request (Gemini 3.8 Live Extended Thinking says "let me
        /// check…", runs a tool without blocking, then answers). Only
        /// `in_progress` is working; `idle` and anything unrecognised read as
        /// not working, so an unknown spelling can never leave "Working…" on
        /// screen. Engines without the signal never send the envelope.
        case interactionStatus(inProgress: Bool)
    }

    /// Build the `session.start` payload from the two INDEPENDENT axes:
    ///
    /// - `engine` selects the provider family (`voice_mode`). Backend
    ///   `hands_free` names the local cascaded STT → chat → TTS provider; it
    ///   does *not* mean "keep the microphone open". Conflating the two once
    ///   made the default iOS Live mode use the cascaded engine instead of GPT
    ///   Realtime.
    /// - `pttOn` selects the turn boundary (`turn_boundary`): an explicit
    ///   push-to-talk commit, or continuous capture with VAD deciding.
    ///
    /// All four combinations are valid. The backend ignores `turn_boundary`
    /// under `hands_free` (it configures no realtime provider there), so in that
    /// mode PTT is enforced by the client gating the PCM stream; the field is
    /// still sent to keep the payload shape uniform.
    static func startPayload(
        uiThreadId: String,
        realtimeProfile: String,
        engine: VoiceEngine,
        pttOn: Bool,
        requireVoicePrefix: Bool? = nil,
        screenLocked: Bool = false
    ) -> [String: Any] {
        var payload = turnBoundaryPayload(pttOn: pttOn)
        payload.merge([
            "realtime_profile": realtimeProfile,
            "voice_mode": engine.voiceMode,
            // VoiceAudioEngine arms voice-processing AEC on its I/O nodes.
            "echo_cancellation": true,
            "screen_locked": screenLocked
        ]) { _, new in new }
        if let requireVoicePrefix {
            payload["require_voice_prefix"] = requireVoicePrefix
        }
        let trimmed = uiThreadId.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { payload["ui_thread_id"] = trimmed }
        return payload
    }

    /// Live input-boundary update sent over the existing control WebSocket.
    /// The client keeps its media registration and audio graph alive while the
    /// backend applies the provider-specific update or upstream rebind.
    static func turnBoundaryPayload(pttOn: Bool) -> [String: Any] {
        ["turn_boundary": pttOn ? "push_to_talk" : "server_vad"]
    }

    static func screenStatePayload(locked: Bool) -> [String: Any] {
        ["locked": locked]
    }

    static func mediaSessionRegistrationBody(
        includeHandsFreeAudio: Bool,
        audioProfile: String?,
        audioStageOptions: [String: String]
    ) -> [String: Any] {
        var body: [String: Any] = [
            "surface_type": "web_mobile",
            "transport": "websocket",
            "capabilities": [
                "mic": true,
                "realtime_voice": true,
                "text_bubble": true
            ],
            "permissions": ["mic": "granted"],
            "display_label": "Magios Voice",
            "user_agent": "Magios-iOS"
        ]
        if includeHandsFreeAudio {
            body["audio_surface"] = "hands_free"
            body["audio_stage_options"] = audioStageOptions
            if let audioProfile, !audioProfile.isEmpty {
                body["audio_profile"] = audioProfile
            }
        }
        return body
    }

    /// Control envelopes must be WebSocket *text* frames. Binary frames on the
    /// same socket are reserved for 24 kHz PCM audio by the backend.
    static func envelopeText(kind: String, payload: [String: Any]) -> String {
        guard let data = try? JSONSerialization.data(
            withJSONObject: ["kind": kind, "payload": payload]
        ) else { return "" }
        return String(data: data, encoding: .utf8) ?? ""
    }

    static func route(kind: String, payload: [String: Any]) -> Event {
        switch kind {
        case "session.ready": return .ready
        case "session.rotating": return .rotating
        case "audio.rebind": return .rebind
        case "session.ended": return .ended
        case "session.error":
            return .error(message: payload["message"] as? String ?? "voice error",
                          recoverable: payload["recoverable"] as? Bool ?? false)
        case "tool.result": return .toolResult
        case "tutor.takeover.started":
            guard payload["client_handoff"] as? Bool == true,
                  let text = payload["text"] as? String,
                  !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                return .ignore
            }
            return .tutorBlackboardRequested(
                text: text,
                quick: payload["quick"] as? Bool ?? false
            )
        case "tutor.takeover.failed":
            return .guidedFlowRejected(
                message: payload["message"] as? String
                    ?? "I couldn't start that guided session. Please try again.",
                backendAnnounced: payload["backend_announced"] as? Bool ?? false
            )
        case "transcript.user.partial":
            return .transcriptPartial(
                role: .user,
                text: payload["text"] as? String ?? "",
                itemID: payload["item_id"] as? String,
                turnGeneration: integer(payload["turn_generation"])
            )
        case "transcript.user":
            return .transcript(
                role: .user,
                text: payload["text"] as? String ?? "",
                itemID: payload["item_id"] as? String,
                turnGeneration: integer(payload["turn_generation"])
            )
        case "transcript.user.ignored":
            return .transcriptIgnored(
                itemID: payload["item_id"] as? String,
                turnGeneration: integer(payload["turn_generation"]),
                reason: payload["reason"] as? String ?? "ignored"
            )
        case "transcript.user.cleared":
            return .transcriptCleared(
                itemID: payload["item_id"] as? String,
                turnGeneration: integer(payload["turn_generation"]),
                reason: payload["reason"] as? String ?? "cleared"
            )
        case "transcript.assistant.delta":
            return .transcriptPartial(
                role: .assistant,
                text: payload["text"] as? String ?? "",
                itemID: nonEmptyString(payload["response_id"]) ?? nonEmptyString(payload["item_id"]),
                turnGeneration: integer(payload["turn_generation"])
            )
        case "transcript.assistant":
            return .transcript(
                role: .assistant,
                text: payload["text"] as? String ?? "",
                itemID: nonEmptyString(payload["response_id"]) ?? nonEmptyString(payload["item_id"]),
                turnGeneration: integer(payload["turn_generation"])
            )
        case "audio.output.ended":
            // `voice_control_handler.rs` sends `{response_id, interrupted}`.
            // A missing flag (older server) reads as a natural drain, which
            // asks nothing of playback — the conservative direction here.
            return .assistantAudioEnded(interrupted: payload["interrupted"] as? Bool ?? false)
        case "response.interrupted":
            // An interrupt by definition. On the cascaded engine this is the
            // ONLY interrupt signal: its barge-in path takes the active
            // response id before the provider's audio-done can match, so no
            // `audio.output.ended{interrupted:true}` ever follows it.
            return .assistantAudioEnded(interrupted: true)
        case "interaction.status":
            let status = (payload["status"] as? String ?? "")
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .lowercased()
            return .interactionStatus(inProgress: status == "in_progress")
        default: return .ignore
        }
    }

    private static func integer(_ value: Any?) -> Int? {
        if let value = value as? Int { return value }
        return (value as? NSNumber)?.intValue
    }

    private static func nonEmptyString(_ value: Any?) -> String? {
        guard let value = value as? String else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    /// Parse the server-owned address gate negotiated in `session.ready`. Older
    /// servers omit it, which deliberately degrades to disabled client display;
    /// the backend remains authoritative for transcript admission.
    /// What the BACKEND resolved this call to be. Display-only.
    ///
    /// There is deliberately no counterpart the client can send: the boundary is
    /// not negotiable, and a control implying otherwise would suggest a shared
    /// room can ask to be trusted. `elevatable` is mirrored from the wire and is
    /// always false; it exists so a reader of this type sees the boundary is
    /// fixed rather than wondering whether some other flow can raise it.
    struct Boundary: Equatable {
        let surface: String
        /// `owner` or `untrusted` — the half that decides what a call may reach.
        let audience: String
        let agentID: String
        let elevatable: Bool

        var isUntrusted: Bool { audience == "untrusted" }
    }

    /// Narrow the wire block without inventing one. A missing or malformed
    /// `boundary` yields `nil` — "we do not know" — never a fabricated owner
    /// boundary, which is the one wrong answer a display could give. An older
    /// backend that does not send it therefore shows nothing rather than
    /// implying the call is private.
    static func boundary(from payload: [String: Any]) -> Boundary? {
        guard let value = payload["boundary"] as? [String: Any] else { return nil }
        let surface = (value["surface"] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let audience = (value["audience"] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !surface.isEmpty, !audience.isEmpty else { return nil }
        return Boundary(
            surface: surface,
            audience: audience,
            agentID: value["agent_id"] as? String ?? "",
            elevatable: false
        )
    }

    static func addressing(from payload: [String: Any]) -> Addressing {
        guard let value = payload["addressing"] as? [String: Any] else { return .disabled }
        let phrases = (value["activation_phrases"] as? [Any] ?? [])
            .compactMap { ($0 as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        return Addressing(
            required: value["required"] as? Bool ?? false,
            activationPhrases: phrases,
            followUpWindowMs: value["follow_up_window_ms"] as? Int ?? 8_000
        )
    }
}

struct RealtimeVoiceCaption: Equatable, Identifiable {
    let id: UUID
    let role: RealtimeVoiceProtocol.Role
    let text: String
    let isFinal: Bool
    let sourceItemID: String?
    let turnGeneration: Int?

    init(
        id: UUID = UUID(),
        role: RealtimeVoiceProtocol.Role,
        text: String,
        isFinal: Bool = true,
        sourceItemID: String? = nil,
        turnGeneration: Int? = nil
    ) {
        self.id = id
        self.role = role
        self.text = text
        self.isFinal = isFinal
        self.sourceItemID = sourceItemID
        self.turnGeneration = turnGeneration
    }
}

struct RealtimeVoiceCaptionState {
    private(set) var captions: [RealtimeVoiceCaption] = []
    private var partialUserCaptionID: UUID?

    mutating func reset() {
        captions = []
        partialUserCaptionID = nil
    }

    mutating func clearUnfinishedUserCaptions() {
        removeUnfinishedUserCaption()
    }

    @discardableResult
    mutating func apply(_ event: RealtimeVoiceProtocol.Event) -> Bool {
        switch event {
        case .transcript(let role, let text, let itemID, let turnGeneration):
            let clean = text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !clean.isEmpty else { return true }
            let matchingIndex = captions.firstIndex {
                !$0.isFinal && $0.role == role && (
                    (itemID != nil && $0.sourceItemID == itemID)
                    || (turnGeneration != nil && $0.turnGeneration == turnGeneration)
                    || (role == .user && itemID == nil && $0.id == partialUserCaptionID)
                )
            }
            if let matchingIndex {
                let id = captions[matchingIndex].id
                captions[matchingIndex] = RealtimeVoiceCaption(
                    id: id,
                    role: role,
                    text: clean,
                    isFinal: true,
                    sourceItemID: itemID ?? captions[matchingIndex].sourceItemID,
                    turnGeneration: turnGeneration ?? captions[matchingIndex].turnGeneration
                )
                if role == .user && partialUserCaptionID == id {
                    partialUserCaptionID = nil
                }
                return true
            }
            captions.append(RealtimeVoiceCaption(
                role: role,
                text: clean,
                sourceItemID: itemID,
                turnGeneration: turnGeneration
            ))
            return true

        case .transcriptPartial(let role, let text, let itemID, let turnGeneration):
            let clean = text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !clean.isEmpty else { return true }
            let matchingIndex = captions.firstIndex {
                !$0.isFinal && $0.role == role && (
                    (itemID != nil && $0.sourceItemID == itemID)
                    || (turnGeneration != nil && $0.turnGeneration == turnGeneration)
                    || (role == .user && itemID == nil && $0.id == partialUserCaptionID)
                )
            }
            let id: UUID
            let merged: String
            if let matchingIndex {
                id = captions[matchingIndex].id
                merged = Self.mergeStreamingCaption(
                    existing: captions[matchingIndex].text,
                    incoming: clean
                )
            } else {
                if role == .user { removeUnfinishedUserCaption() }
                id = UUID()
                merged = clean
            }
            if role == .user {
                partialUserCaptionID = id
            }
            let caption = RealtimeVoiceCaption(
                id: id,
                role: role,
                text: merged,
                isFinal: false,
                sourceItemID: itemID ?? matchingIndex.flatMap { captions[$0].sourceItemID },
                turnGeneration: turnGeneration ?? matchingIndex.flatMap { captions[$0].turnGeneration }
            )
            if let index = captions.firstIndex(where: { $0.id == id }) {
                captions[index] = caption
            } else {
                captions.append(caption)
            }
            return true

        case .transcriptCleared(let itemID, let turnGeneration, _),
             .transcriptIgnored(let itemID, let turnGeneration, _):
            captions.removeAll { caption in
                guard !caption.isFinal, caption.role == .user else { return false }
                if let itemID { return caption.sourceItemID == itemID }
                if let turnGeneration { return caption.turnGeneration == turnGeneration }
                return caption.id == partialUserCaptionID
            }
            if !captions.contains(where: { $0.id == partialUserCaptionID }) {
                partialUserCaptionID = nil
            }
            return true

        default:
            return false
        }
    }

    private mutating func removeUnfinishedUserCaption() {
        captions.removeAll { !$0.isFinal && $0.role == .user }
        partialUserCaptionID = nil
    }

    private static func mergeStreamingCaption(existing: String, incoming: String) -> String {
        if incoming.isEmpty { return existing }
        if existing.isEmpty { return incoming }
        if incoming.hasPrefix(existing) { return incoming }
        if existing.hasPrefix(incoming) { return existing }
        return existing + incoming
    }
}
