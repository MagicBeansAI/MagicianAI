import Foundation
import AVFoundation

enum SpeechPlaybackResult: Equatable {
    case completed, cancelled, failed, skipped
}

enum SpeechFocusPolicy: Equatable {
    /// Ordinary narration must not speak over a Live/Hands-free call.
    case respectVoiceCall
    /// Ambient Dictation owns the ambient window's focus token and uses the
    /// shared synthesizer as that conversation's sole reply path.
    case ambientDictationOwner
    /// A fenced delivery owns the quiet interval of an existing call. It borrows
    /// that call's full-duplex audio session and must never reconfigure/release it.
    case concurrentVoiceOwner
}

/// Text-to-speech for spoken assistant replies. Per the TTS setting it either
/// speaks **on-device** (`AVSpeechSynthesizer` — free, offline, no server cost) or
/// via **Magician** (`POST /media/tts/synthesize` → play the returned audio with
/// the backend's configured voice). Magician failures fall back to on-device so a
/// reply is always spoken. Reads a markdown-stripped version so it sounds like
/// prose, not symbols/code.
final class SpeechSynthesizer: NSObject, ObservableObject, AVSpeechSynthesizerDelegate, AVAudioPlayerDelegate {
    static let shared = SpeechSynthesizer()

    // Lazy so a plain `SpeechSynthesizer.shared` access (e.g. a test creating a
    // ChatViewModel) doesn't spin up TTS + its voice-asset queries. Under XCTest,
    // speak()/stop() short-circuit before ever touching this, so it's never made.
    private lazy var synth: AVSpeechSynthesizer = {
        let synthesizer = AVSpeechSynthesizer()
        synthesizer.delegate = self
        return synthesizer
    }()
    private var player: AVAudioPlayer?
    private var activeUtterance: AVSpeechUtterance?
    private var playbackGeneration = 0
    private var playbackStarted: (() -> Void)?
    private var playbackCompleted: ((SpeechPlaybackResult) -> Void)?
    /// Whether this object currently has the shared `AVAudioSession` configured
    /// for playback — i.e. whether it has anything to hand back.
    ///
    /// **It exists because `finish()` used to deactivate unconditionally, and
    /// `stop()` calls `finish()` even when nothing was speaking.** Chat calls
    /// `stop()` routinely as a hush — on send, on barge-in, on leaving a thread —
    /// and each of those was a `setActive(false, .notifyOthersOnDeactivation)`
    /// against a session this object had never taken. That is survivable for every
    /// other feature and fatal for one: an armed ambient window's session cannot be
    /// reactivated from the background (Apple DTS 826462), so a user who armed a
    /// window and then sent one chat message lost the window — not visibly, but at
    /// their next wake word, off screen, with the orb still saying they were being
    /// heard. `speak()` already refuses while `VoiceCallAudioFocus` is held, which
    /// covered the *speaking* half and left the hush uncovered.
    ///
    /// Design §15 named this defect ("deactivates **even when nothing is
    /// speaking**") and recorded the row as resolved on the strength of the ambient
    /// call's own hush being gated. The chat-side callers were not covered by that.
    private var holdsSession = false
    private var borrowsVoiceSession = false

    /// Deactivations actually issued. `internal` so the invariant is assertable —
    /// same idiom, and the same reason, as `VoiceAudioEngine.sessionReleaseCount`:
    /// "it did not deactivate" is not observable through any `AVAudioSession` API,
    /// and this is the one property whose wrong value is a microphone that cannot
    /// be reopened.
    private(set) var sessionReleaseCount = 0

    /// Re-categorisations actually performed, and `internal` for the same reason
    /// `sessionReleaseCount` is: "it left the shared session alone" is not observable
    /// through any `AVAudioSession` API, and this is the site that swaps the category
    /// to one with no microphone input.
    private(set) var sessionConfigureCount = 0

    /// The armed-ambient-window rail — the same seam its three neighbours carry. See
    /// `AmbientRail`, and `configureSession` for what it changes.
    var ambientRail = AmbientRail.live

    @Published var isSpeaking = false
    /// The source that actually started playback, including a local fallback.
    /// Nil while preparing or idle; the selected preference alone cannot say
    /// which voice the listener is hearing.
    @Published private(set) var playbackSource: TTSEngine?
    /// The message currently being read aloud (drives the per-message speak
    /// button's active state). nil when idle or speaking a non-message utterance.
    @Published var activeMessageId: String?

    private override init() {
        super.init()
    }

    /// Speak `text`; `messageId` (when given) lights up that message's speak
    /// button and lets a second tap stop it.
    @discardableResult
    func speak(
        _ text: String,
        messageId: String? = nil,
        focusPolicy: SpeechFocusPolicy = .respectVoiceCall,
        onStart: (() -> Void)? = nil,
        completion: ((SpeechPlaybackResult) -> Void)? = nil
    ) -> Bool {
        // Read only the `<speech>` portion when present (else the whole body),
        // then strip markdown so it sounds like prose — matching the web.
        // No audio under XCTest — keeps AVSpeechSynthesizer (and its voice-asset
        // warnings) out of the test console.
        guard !isRunningUnderTests else {
            completion?(.skipped)
            return false
        }
        // A realtime Live call owns audio output and the shared AVAudioSession
        // (see VoiceCallAudioFocus). Never speak on top of it — this also keeps
        // configureSession()/finish() from seizing the session away from the
        // live capture engine. Belt-and-suspenders vs the ChatViewModel guard.
        guard Self.permitsSpeech(
            voiceCallFocusActive: VoiceCallAudioFocus.shared.isActive,
            policy: focusPolicy
        ) else {
            completion?(.skipped)
            return false
        }
        let clean = Self.stripMarkdown(SpeechTags.spokenText(text))
        guard !clean.isEmpty else {
            completion?(.skipped)
            return false
        }
        stop()
        borrowsVoiceSession = focusPolicy == .concurrentVoiceOwner && VoiceCallAudioFocus.shared.isActive
        playbackGeneration += 1
        let generation = playbackGeneration
        playbackStarted = onStart
        playbackCompleted = completion
        activeMessageId = messageId
        switch AudioSettings.shared.ttsEngine {
        case .onDevice: speakOnDevice(clean, generation: generation)
        case .magician: speakViaMagician(clean, generation: generation)
        }
        return true
    }

    static func permitsSpeech(
        voiceCallFocusActive: Bool,
        policy: SpeechFocusPolicy
    ) -> Bool {
        !voiceCallFocusActive || policy == .ambientDictationOwner || policy == .concurrentVoiceOwner
    }

    func isSpeaking(messageId: String) -> Bool { activeMessageId == messageId }

    func stop() {
        playbackGeneration += 1
        activeUtterance = nil
        // Guard the synth access so `stop()` never lazily creates it under XCTest.
        if !isRunningUnderTests, synth.isSpeaking { synth.stopSpeaking(at: .immediate) }
        player?.stop()
        player = nil
        finish(.cancelled)
    }

    // MARK: On-device (AVSpeechSynthesizer)

    private func speakOnDevice(_ text: String, generation: Int) {
        guard generation == playbackGeneration else { return }
        configureSession()
        let utterance = AVSpeechUtterance(string: text)
        utterance.rate = AVSpeechUtteranceDefaultSpeechRate
        activeUtterance = utterance
        synth.speak(utterance)
        isSpeaking = true
    }

    // MARK: Magician (backend TTS → play audio)

    private func speakViaMagician(_ text: String, generation: Int) {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/media/tts/synthesize") else {
            speakOnDevice(text, generation: generation); return
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        let audio = AudioSettings.shared
        var body: [String: Any] = [
            "text": text
        ]
        if let profile = audio.requestProfile(for: .dictation) {
            body["audio_profile"] = profile
        }
        if let option = audio.requestStageOptions(for: .dictation)[NativeAudioStage.tts.rawValue] {
            body["audio_stage_options"] = [NativeAudioStage.tts.rawValue: option]
        }
        request.httpBody = try? JSONSerialization.data(withJSONObject: body)
        isSpeaking = true
        URLSession.shared.dataTask(with: request) { [weak self] data, response, _ in
            guard let self = self else { return }
            let ok = (response as? HTTPURLResponse).map { (200...299).contains($0.statusCode) } ?? false
            DispatchQueue.main.async {
                guard generation == self.playbackGeneration else { return }
                if ok, let data = data, !data.isEmpty {
                    self.playAudio(data, fallbackText: text, generation: generation)
                } else {
                    self.speakOnDevice(text, generation: generation)  // backend unavailable → on-device
                }
            }
        }.resume()
    }

    private func playAudio(_ data: Data, fallbackText: String, generation: Int) {
        guard generation == playbackGeneration else { return }
        configureSession()
        do {
            let p = try AVAudioPlayer(data: data)
            p.delegate = self
            player = p
            if p.play() {
                playbackSource = .magician
                isSpeaking = true
                notifyPlaybackStarted()
            } else {
                player = nil
                speakOnDevice(fallbackText, generation: generation)
            }
        } catch {
            speakOnDevice(fallbackText, generation: generation)  // couldn't decode → on-device
        }
    }

    /// Take the shared session for playback — **unless an ambient window is
    /// armed.**
    ///
    /// **This is a re-categorisation, and that is the hazard rather than the
    /// activation.** `.playback` has no *input*, so swapping the shared session to
    /// it takes the microphone out from under a live ambient tap — the same shape as
    /// `BackgroundEngine.playSilence`, and the symptom is identical: a wake word
    /// that silently stops working rather than an error anywhere.
    ///
    /// **`speak()`'s audio-focus guard does not cover this**, which is why the rail
    /// belongs here rather than only at the entry point. The Magician TTS path defers:
    /// `speak` → `speakViaMagician` → a URLSession round trip → `DispatchQueue.main.async`
    /// → `playAudio` → here. `AmbientController.arm` can run start to finish inside
    /// that round trip — trivially, now that Settings has an in-app arm button — so
    /// the focus check passed against a world that no longer exists by the time this
    /// runs.
    ///
    /// Refusing rather than deferring is what keeps `holdsSession` honest: the latch
    /// stays false, so `finish()` cannot then deactivate a session this object never
    /// took. Without that, `abandonSessionClaim()`'s work is undone one line later by
    /// `holdsSession = true` and the window dies at `finish()`.
    ///
    /// The reply still plays. Whatever session ambient has configured is a
    /// `.playAndRecord` one routed to the speaker, so the utterance is audible; only
    /// the ducking is not applied. Cancelling an utterance already in the air would be
    /// a larger behaviour change for a smaller gain.
    ///
    /// `internal` and counted so the rail is assertable at all. `MainActor.assumeIsolated`
    /// rather than an `@MainActor` annotation for the reason `DictationController.releaseSession`
    /// gives: every caller here is already on the main queue, several of them because a
    /// `DispatchQueue.main.async` put them there, and those cannot call main-actor-isolated
    /// code without one.
    func configureSession() {
        if borrowsVoiceSession && VoiceCallAudioFocus.shared.isActive { return }
        let ambientWindowIsLive = MainActor.assumeIsolated { ambientRail.windowIsLive() }
        guard !ambientWindowIsLive else {
            debugLog("[tts] leaving the audio session alone: an ambient window is armed")
            return
        }
        sessionConfigureCount += 1
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, mode: .spokenAudio, options: [.duckOthers])
        try? session.setActive(true)
        holdsSession = true
    }

    /// Give up the claim on the shared session **without deactivating it.**
    ///
    /// Called by `AmbientController.arm` the instant the ambient tap is live. By
    /// then this object's claim is void in fact — ambient has reconfigured the
    /// session to `.playAndRecord` and activated it — and this makes it void in
    /// bookkeeping too, so a reply that was already playing cannot hand back a
    /// session it no longer holds. See `finish`.
    ///
    /// **A backstop, not the fix.** On its own it is a snapshot that the deferred
    /// TTS path re-arms: it neither bumps `playbackGeneration` nor stops the
    /// synthesizer, so a callback landing after it calls `configureSession()` and
    /// sets `holdsSession` straight back to true. `configureSession`'s own rail is
    /// what closes that; this covers the window between `arm` and the next callback.
    func abandonSessionClaim() {
        holdsSession = false
    }

    // MARK: Delegates

    func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didStart utterance: AVSpeechUtterance) {
        guard utterance === activeUtterance else { return }
        playbackSource = .onDevice
        notifyPlaybackStarted()
    }
    func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didFinish utterance: AVSpeechUtterance) {
        guard utterance === activeUtterance else { return }
        finish(.completed)
    }
    func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didCancel utterance: AVSpeechUtterance) {
        guard utterance === activeUtterance else { return }
        finish(.cancelled)
    }
    func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        guard player === self.player else { return }
        finish(flag ? .completed : .failed)
    }

    private func notifyPlaybackStarted() {
        let callback = playbackStarted
        playbackStarted = nil
        callback?()
    }

    private func finish(_ result: SpeechPlaybackResult) {
        let completion = playbackCompleted
        playbackStarted = nil
        playbackCompleted = nil
        activeUtterance = nil
        player = nil
        isSpeaking = false
        playbackSource = nil
        activeMessageId = nil
        // Hand the session back only if this object actually took it. Synchronous
        // and local on purpose: the deactivation cannot be deferred to a main-actor
        // hop, because chat's barge-in calls `stop()` and then immediately starts
        // dictation, whose own `setActive(true)` a queued deactivation would land
        // on top of. See `holdsSession`.
        if holdsSession {
            holdsSession = false
            sessionReleaseCount += 1
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        }
        completion?(result)
    }

    /// Strip common markdown so the reply is spoken as prose, not symbols/code.
    static func stripMarkdown(_ input: String) -> String {
        var s = input
        let subs: [(String, String)] = [
            ("```[\\s\\S]*?```", " "),          // fenced code blocks
            ("`([^`]*)`", "$1"),                  // inline code
            ("!\\[[^\\]]*\\]\\([^)]*\\)", " "),   // images
            ("\\[([^\\]]*)\\]\\([^)]*\\)", "$1"), // links → link text
            ("^#{1,6}\\s*", ""),                   // headers
            ("[*_~]{1,3}", ""),                    // emphasis markers
            ("^>\\s?", ""),                         // blockquotes
            ("^\\s*[-*+]\\s+", ""),                // bullet markers
            ("\\|", " ")                            // table pipes
        ]
        for (pattern, replacement) in subs {
            s = s.replacingOccurrences(
                of: pattern, with: replacement,
                options: [.regularExpression, .caseInsensitive]
            )
        }
        return s.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
