import Foundation
import Combine
import AVFoundation
import Speech
import UIKit
import os

/// Records a short voice note and transcribes it. Per the audio setting, it tries
/// Apple's **on-device** `SFSpeechRecognizer` first (free, offline, private) and
/// falls back to the backend STT endpoint (`POST /media/stt/transcribe`) only when
/// on-device is unavailable or yields nothing and the selected policy permits
/// cloud STT. Ordinary callers receive the
/// transcript for the composer; Ambient Dictation reuses the file-based capture
/// as one bounded turn in its record → STT → agent → TTS conversation loop.
/// Chat mic captures are ephemeral by default. When the user explicitly enables
/// Audio Notes, `startVoiceNote` records a file and independently archives it
/// through the scoped Notes provider.
/// Realtime duplex voice (WebRTC) remains a separate transport.
final class DictationController: NSObject, ObservableObject {
    @Published var isRecording = false
    @Published var isTranscribing = false
    /// Live on-device partials, updated on the main queue while streaming. Empty for
    /// the file→cloud path (no partials there).
    @Published var partialTranscript: String = ""
    /// Set when a required permission is off/denied → the UI shows an alert with an
    /// "Open Settings" action (iOS only re-prompts once; after that it's Settings).
    @Published var permissionAlert: PermissionAlert?

    struct PermissionAlert: Identifiable {
        let id = UUID(); let title: String; let message: String
    }

    private var recorder: AVAudioRecorder?
    private var fileURL: URL?
    private let principal = MagicianAccess.principal
    private let workspace = MagicianAccess.workspace
    private let recognizer = SFSpeechRecognizer(locale: Locale(identifier: "en-US"))

    // The capture route is latched only after its recorder/engine has started.
    // Finalization must stop that exact route; inferring from available models or
    // a leftover boolean can strand an externally started recording.
    private var captureRoute: DictationCaptureRoute = .idle

    /// The armed-ambient-window rail. See `AmbientRail` for why this type has to
    /// care, and `startLive` / `releaseSession` for the two things it does about it.
    var ambientRail = AmbientRail.live

    /// Deactivations actually issued. See `releaseSession`.
    private(set) var sessionReleaseCount = 0

    // Live on-device streaming state (hold-to-talk). See `startLive`/`finishLive`.
    private var audioEngine: AVAudioEngine?
    private var liveRequest: SFSpeechAudioBufferRecognitionRequest?
    private var liveTask: SFSpeechRecognitionTask?
    private var finishCompletion: ((String?) -> Void)?
    private var finishFallbackWork: DispatchWorkItem?

    // File transcription outlives the recorder that produced it. Keep explicit
    // ownership of both asynchronous transports and the temporary file so an
    // ambient end/deadline can cancel the whole rail. The lease generation also
    // makes late Speech/URLSession callbacks harmless after cancellation.
    private var transcriptionLease = DictationTranscriptionLease()
    private var transcriptionCompletion: ((String?) -> Void)?
    private var onDeviceTranscriptionTask: SFSpeechRecognitionTask?
    private var cloudTranscriptionTask: URLSessionDataTask?

    // Explicit Chat voice notes are always file-backed, even when Apple's live
    // partial transcription is available. That gives the Notes outbox a real
    // recording to retain while STT and the cancelable composer countdown carry
    // on independently. Ambient turns and Thinking Map dictation remain
    // conversational input and are not archived as personal Audio Notes.
    private let audioNoteUploadQueue = AudioNoteUploadQueue.shared
    private var shouldArchiveCurrentCapture = false
    private var voiceNoteCapturedAt: Date?
    private var pendingAudioNote: (generation: Int, id: String)?

    // Ambient Dictation deliberately buffers bounded PCM turns even when ordinary
    // composer dictation can stream Apple partials. Each completed buffer becomes
    // a final WAV for the selected recording-STT profile, while one input graph
    // remains alive across the complete ambient conversation.
    private var ambientCaptureGeneration = 0
    private var ambientStartCompletion: ((Bool) -> Void)?
    private var ambientSpeechBegan: (() -> Void)?
    private var ambientResultCompletion: ((AmbientDictationCaptureResult) -> Void)?
    private var ambientInputReadyWork: DispatchWorkItem?
    private var ambientMeterTimer: Timer?
    private var ambientSilenceGate: AmbientDictationSilenceGate?
    /// One input graph lives for the complete Ambient Dictation conversation.
    /// The graph keeps iOS background audio admission; this collector alone is
    /// opened and closed at turn boundaries. No recorder or audio queue is
    /// allocated after the app has moved to the background.
    private var ambientMic: AmbientMicEngine?
    private let ambientPCM = AmbientDictationPCMAccumulator()

    /// Starting an `AVAudioEngine` is not proof that its input render callback is
    /// advancing. In particular, a physical iPhone can report a successful start
    /// after sequential TTS while delivering no microphone frames. Keep this
    /// admission deadline short so the sink can use its bounded rebuild/retry rail
    /// rather than spending the entire eight-second follow-up window in a deaf
    /// state.
    nonisolated static let ambientFirstFrameTimeoutSeconds: TimeInterval = 1.5

    @MainActor
    private func requestAndStart(live: Bool = false, ambientGeneration: Int? = nil) {
        // No capture under XCTest — the same guard, and the same reason, as
        // `SpeechSynthesizer.speak` and `BackgroundEngine.connectWebSocket`. Without
        // it a test that calls `startLive()` either raises a real microphone
        // permission prompt over the test host (permission undetermined) or actually
        // starts recording to a file and installs an `AVAudioEngine` tap (permission
        // already granted) — neither deterministic, and one of them leaves a modal
        // alert behind for whatever runs next.
        //
        // It is placed HERE rather than in `startLive` deliberately: `startLive`'s
        // guards and the ambient rail below it are exactly what wants asserting, and
        // a guard one level up would make them unreachable from a test.
        guard !isRunningUnderTests else {
            resolveAmbientStart(false, generation: ambientGeneration)
            return
        }
        // Microphone is required to record at all — handle every status.
        switch AVAudioApplication.shared.recordPermission {
        case .granted:
            ensureSpeechThenRecord(live: live, ambientGeneration: ambientGeneration)
        case .undetermined:
            let owner = WeakDictationController(self)
            AVAudioApplication.requestRecordPermission { [owner] granted in
                DispatchQueue.main.async {
                    guard let self = owner.value else { return }
                    guard self.acceptsAmbientGeneration(ambientGeneration) else { return }
                    if granted {
                        self.ensureSpeechThenRecord(
                            live: live,
                            ambientGeneration: ambientGeneration
                        )
                    } else {
                        self.micDeniedAlert()
                        self.resolveAmbientStart(false, generation: ambientGeneration)
                    }
                }
            }
        case .denied:
            micDeniedAlert()
            resolveAmbientStart(false, generation: ambientGeneration)
        @unknown default:
            micDeniedAlert()
            resolveAmbientStart(false, generation: ambientGeneration)
        }
    }

    /// Request speech-recognition auth when on-device STT may be used, then record.
    /// A speech denial isn't fatal — cloud STT covers it unless the user forced
    /// "On-device only", in which case we surface a permission alert.
    @MainActor
    private func ensureSpeechThenRecord(
        live: Bool = false,
        ambientGeneration: Int? = nil
    ) {
        guard acceptsAmbientGeneration(ambientGeneration) else { return }
        let source = AudioSettings.shared.sttSource
        guard source.prefersOnDevice else {
            startCapture(live: live, ambientGeneration: ambientGeneration)
            return
        }
        switch SFSpeechRecognizer.authorizationStatus() {
        case .authorized, .restricted:
            startCapture(live: live, ambientGeneration: ambientGeneration)
        case .notDetermined:
            SFSpeechRecognizer.requestAuthorization { [weak self] status in
                DispatchQueue.main.async {
                    guard let self = self else { return }
                    guard self.acceptsAmbientGeneration(ambientGeneration) else { return }
                    if status == .denied && !source.allowsCloud {
                        self.speechDeniedAlert()
                        self.resolveAmbientStart(false, generation: ambientGeneration)
                    } else {
                        // Authorized, or cloud fallback available.
                        self.startCapture(live: live, ambientGeneration: ambientGeneration)
                    }
                }
            }
        case .denied:
            if source.allowsCloud {
                startCapture(live: live, ambientGeneration: ambientGeneration)
            } else {
                speechDeniedAlert()
                resolveAmbientStart(false, generation: ambientGeneration)
            }
        @unknown default:
            startCapture(live: live, ambientGeneration: ambientGeneration)
        }
    }

    /// Route to the live streaming path only when live was requested and on-device STT
    /// is both preferred and available right now; otherwise use the file recorder (whose
    /// transcript is produced on stop, so the latched `.file` route finalizes via
    /// `stopAndTranscribe`).
    @MainActor
    private func startCapture(live: Bool, ambientGeneration: Int? = nil) {
        guard acceptsAmbientGeneration(ambientGeneration) else { return }
        if let ambientGeneration {
            beginAmbientCaptureEngine(generation: ambientGeneration)
            return
        }
        if live, Self.shouldUseLive(prefersOnDevice: AudioSettings.shared.sttSource.prefersOnDevice,
                                    onDeviceAvailable: onDeviceAvailable) {
            startLiveStreaming(ambientGeneration: ambientGeneration)
        } else {
            // File path; captureRoute stays `.file`, so finishLive resolves via
            // the selected recording-STT pipeline.
            beginRecording(ambientGeneration: ambientGeneration)
        }
    }

    private func micDeniedAlert() {
        permissionAlert = PermissionAlert(
            title: "Microphone Access Off",
            message: "Magican needs the microphone to dictate. Turn it on in Settings.")
    }
    private func speechDeniedAlert() {
        permissionAlert = PermissionAlert(
            title: "Speech Recognition Off",
            message: "On-device dictation needs Speech Recognition. Turn it on in Settings, or switch dictation to Magican in the composer's voice menu.")
    }

    /// Open the app's page in the iOS Settings app so the user can flip a denied
    /// permission back on.
    static func openSettings() {
        if let url = URL(string: UIApplication.openSettingsURLString) {
            UIApplication.shared.open(url)
        }
    }

    private func beginRecording(ambientGeneration: Int? = nil) {
        guard acceptsAmbientGeneration(ambientGeneration) else { return }
        // An ambient window already activated and owns the shared `.voiceChat`
        // session in the foreground. Re-categorising or re-activating it here can
        // be refused after a locked/background wake and destroys the window's
        // return path. Ordinary composer Dictation still owns and configures its
        // session exactly as before.
        if ambientGeneration == nil {
            let session = AVAudioSession.sharedInstance()
            do {
                try session.setCategory(
                    .playAndRecord,
                    mode: .spokenAudio,
                    options: [.defaultToSpeaker, .allowBluetoothHFP]
                )
                try session.setActive(true)
            } catch {
                debugLog("[dictation] audio session error: \(error.localizedDescription)")
                resolveAmbientStart(false, generation: ambientGeneration)
                return
            }
        }
        let fileExtension = "m4a"
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(
            "dictation-\(UUID().uuidString).\(fileExtension)"
        )
        let settings: [String: Any]
        settings = [
            AVFormatIDKey: Int(kAudioFormatMPEG4AAC),
            AVSampleRateKey: 16_000,
            AVNumberOfChannelsKey: 1,
            AVEncoderAudioQualityKey: AVAudioQuality.medium.rawValue,
        ]
        do {
            let rec = try AVAudioRecorder(url: url, settings: settings)
            rec.isMeteringEnabled = false
            guard rec.record() else {
                try? FileManager.default.removeItem(at: url)
                releaseSession()
                debugLog("[dictation] recorder did not start")
                resolveAmbientStart(false, generation: ambientGeneration)
                return
            }
            recorder = rec
            fileURL = url
            captureRoute = .file
            if ambientGeneration == nil, shouldArchiveCurrentCapture {
                voiceNoteCapturedAt = Date()
            }
            isRecording = true
            resolveAmbientStart(true, generation: ambientGeneration)
        } catch {
            debugLog("[dictation] recorder error: \(error.localizedDescription)")
            releaseSession()
            resolveAmbientStart(false, generation: ambientGeneration)
        }
    }

    /// Starts the only microphone graph Ambient Dictation will use for this
    /// conversation. `AmbientMicEngine` produces 16 kHz mono PCM16 and, unlike
    /// `AVAudioRecorder`, can remain alive while individual turns are buffered,
    /// transcribed, and spoken. The input callback always stays installed; the
    /// accumulator decides whether frames belong to the current user turn.
    @MainActor
    private func beginAmbientCaptureEngine(generation: Int) {
        guard acceptsAmbientGeneration(generation) else { return }
        let mic = ambientMic ?? AmbientMicEngine()
        let needsStart = ambientMic == nil
        ambientMic = mic
        ambientPCM.beginTurn()

        if needsStart {
            let collector = ambientPCM
            let owner = WeakDictationController(self)
            do {
                try mic.start(
                    onFrame: { [weak collector, owner] frame in
                        guard collector?.append(frame) == true else { return }
                        DispatchQueue.main.async { [owner] in
                            owner.value?.ambientInputDeliveredFirstFrame()
                        }
                    },
                    onFailure: { [weak self] error in self?.handleAmbientMicFailure(error) }
                )
            } catch {
                ambientPCM.reset()
                ambientMic = nil
                debugLog("[dictation] ambient input graph failed: \(error.localizedDescription)")
                resolveAmbientStart(false, generation: generation)
                return
            }
        }

        captureRoute = .file
        isRecording = true
        debugLog("[dictation] ambient input graph awaiting first frame generation=\(generation)")
        awaitAmbientInputFrames(generation: generation, isFollowUp: false)
    }

    @MainActor
    private func resumeAmbientCaptureEngine(generation: Int) {
        guard acceptsAmbientGeneration(generation),
              let ambientMic,
              captureRoute == .file else {
            resolveAmbientStart(false, generation: generation)
            return
        }
        do {
            // AVSpeechSynthesizer can leave an input engine nominally running
            // while its render callback has stopped. Re-prime that same graph
            // synchronously before accepting another turn; the shared audio
            // session remains active and is never reconfigured here.
            try ambientMic.refreshAfterPlayback()
        } catch {
            debugLog("[dictation] ambient input graph did not recover after playback: \(error.localizedDescription)")
            teardownRetainedAmbientCapture()
            resolveAmbientStart(false, generation: generation)
            return
        }
        ambientPCM.beginTurn()
        isRecording = true
        debugLog("[dictation] ambient follow-up input graph awaiting first frame generation=\(generation)")
        awaitAmbientInputFrames(generation: generation, isFollowUp: true)
    }

    /// Admit a capture turn only after its continuously installed input callback
    /// has delivered PCM for *this* open accumulator gate. A stale frame from the
    /// prior turn cannot satisfy this: the accumulator returns `true` exactly once
    /// after each `beginTurn`, and discards every frame while its gate is closed.
    @MainActor
    private func ambientInputDeliveredFirstFrame() {
        let generation = ambientCaptureGeneration
        guard acceptsAmbientGeneration(generation),
              ambientStartCompletion != nil,
              ambientPCM.hasFrames else { return }
        ambientInputReadyWork?.cancel()
        ambientInputReadyWork = nil
        debugLog("[dictation] ambient input graph delivered first frame generation=\(generation)")
        resolveAmbientStart(true, generation: generation)
        beginAmbientMetering(generation: generation)
    }

    @MainActor
    private func awaitAmbientInputFrames(generation: Int, isFollowUp: Bool) {
        ambientInputReadyWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self else { return }
            MainActor.assumeIsolated {
                guard self.acceptsAmbientGeneration(generation),
                      self.ambientStartCompletion != nil else { return }
                let phase = isFollowUp ? "follow-up" : "initial"
                debugLog(
                    "[dictation] ambient \(phase) input graph produced no frames generation=\(generation)"
                )
                // Retain the graph and its active-session admission. The sink's
                // bounded retry re-enters `refreshAfterPlayback`, which performs a
                // hard tap rebuild without attempting a forbidden background
                // `AVAudioSession.setActive(true)`.
                self.ambientPCM.reset()
                self.isRecording = false
                self.resolveAmbientStart(false, generation: generation)
            }
        }
        ambientInputReadyWork = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + Self.ambientFirstFrameTimeoutSeconds,
            execute: work
        )
    }

    @MainActor
    private func handleAmbientMicFailure(_ error: Error) {
        guard ambientMic != nil else { return }
        let generation = ambientCaptureGeneration
        debugLog("[dictation] ambient input graph failed mid-call: \(error.localizedDescription)")
        teardownRetainedAmbientCapture()
        completeAmbientCapture(.failedToTranscribe, generation: generation)
    }

    private func stopAndTranscribe(completion: @escaping (String?) -> Void) {
        let durationMS = recorder.map { max(0, Int(($0.currentTime * 1_000).rounded())) }
        let shouldArchive = shouldArchiveCurrentCapture
        let capturedAt = voiceNoteCapturedAt
        shouldArchiveCurrentCapture = false
        voiceNoteCapturedAt = nil
        recorder?.stop()
        recorder = nil
        captureRoute = .idle
        isRecording = false
        releaseSession()
        guard let url = fileURL else { completion(nil); return }
        fileURL = nil
        isTranscribing = true
        let generation = transcriptionLease.begin(fileURL: url)
        transcriptionCompletion = completion
        if shouldArchive, let capturedAt {
            audioNoteUploadQueue.stageRecording(
                at: url,
                capturedAt: capturedAt,
                durationMS: durationMS
            ) { [weak self] result in
                guard let self else { return }
                if case .success(let id) = result {
                    self.pendingAudioNote = (generation, id)
                } else if case .failure(let error) = result {
                    self.permissionAlert = PermissionAlert(
                        title: "Audio Note Was Not Saved",
                        message: error.localizedDescription
                    )
                }
                self.beginFileTranscription(url: url, generation: generation)
            }
            return
        }

        beginFileTranscription(url: url, generation: generation)
    }

    private func beginFileTranscription(url: URL, generation: Int) {
        guard transcriptionLease.accepts(generation) else { return }
        let source = AudioSettings.shared.sttSource
        // Respect the selected privacy boundary. In particular, an unavailable
        // Apple recognizer must not turn "On-device only" into a cloud upload.
        switch Self.transcriptionRoute(
            source: source,
            onDeviceAvailable: onDeviceAvailable
        ) {
        case .onDevice(let allowsCloudFallback):
            guard transcriptionLease.beginOnDevice(generation) else {
                finishTranscription(nil, generation: generation)
                return
            }
            transcribeOnDevice(
                url,
                generation: generation,
                allowsCloudFallback: allowsCloudFallback
            )
        case .cloud:
            beginCloudTranscription(generation: generation)
        case .unavailable:
            finishTranscription(nil, generation: generation)
        }
    }

    static func transcriptionRoute(
        source: STTSource,
        onDeviceAvailable: Bool
    ) -> DictationTranscriptionRoute {
        if source.prefersOnDevice, onDeviceAvailable {
            return .onDevice(allowsCloudFallback: source.allowsCloud)
        }
        if source.allowsCloud { return .cloud }
        return .unavailable
    }

    /// Live on-device streaming is used only when on-device STT is both preferred and
    /// available right now; everything else uses the file-transcription path,
    /// whose explicit policy decides whether cloud STT is allowed.
    static func shouldUseLive(prefersOnDevice: Bool, onDeviceAvailable: Bool) -> Bool {
        prefersOnDevice && onDeviceAvailable
    }

    /// Hold-to-talk begins. Streams on-device partials when possible; otherwise records to
    /// a file (transcript is produced on `finishLive`, no partials). Reuses the existing
    /// mic/speech permission gating.
    ///
    /// **`@MainActor` because of the ambient rail below**, which ends a live
    /// microphone synchronously and therefore may not be reached from anywhere
    /// else. Every caller is a SwiftUI gesture handler, so this annotates what was
    /// already true.
    ///
    /// The rail fires HERE — at the single public entry — rather than at each of
    /// the three places this path deactivates the shared audio session, and that
    /// is the point: the deactivations are all downstream of this call, so one
    /// yield covers them, and a user who has pressed hold-to-talk has already
    /// decided they want the microphone for dictation. It fires before the
    /// permission prompts rather than after, which means a dictation the user then
    /// denies still costs them the ambient window; that is the honest order,
    /// because the alternative is three yields racing three capture paths, and the
    /// orb says which of their own actions closed the window.
    @MainActor
    func startLive() {
        guard captureRoute == .idle, !isRecording, !isTranscribing else { return }
        ambientRail.yield(AmbientYieldReason.dictationStarted)
        shouldArchiveCurrentCapture = false
        voiceNoteCapturedAt = nil
        cancelFinishFallback()
        isTranscribing = false
        partialTranscript = ""
        requestAndStart(live: true)
    }

    /// Start a durable Chat voice note. Unlike live composer dictation this is
    /// intentionally file-backed: the recording is copied into a small local
    /// outbox on stop, transcribed through the selected STT route, and uploaded
    /// to the scoped Notes provider without delaying the composer or auto-send.
    @MainActor
    func startVoiceNote() {
        guard captureRoute == .idle, !isRecording, !isTranscribing else { return }
        ambientRail.yield(AmbientYieldReason.dictationStarted)
        shouldArchiveCurrentCapture = true
        voiceNoteCapturedAt = nil
        cancelFinishFallback()
        isTranscribing = false
        partialTranscript = ""
        requestAndStart(live: false)
    }

    /// Start one automatic Ambient Dictation utterance without yielding the
    /// ambient window that owns it. The capture ends after detected speech goes
    /// quiet, after a bounded utterance cap, or after the no-speech follow-up
    /// deadline. Every completion is one-shot and delivered on the main actor.
    @MainActor
    func startAmbientCapture(
        onSpeechBegan: @escaping () -> Void,
        onResult: @escaping (AmbientDictationCaptureResult) -> Void,
        onStarted: @escaping (Bool) -> Void
    ) {
        let startAction = Self.ambientStartAction(
            captureRoute: captureRoute,
            retainedInputGraph: ambientMic != nil,
            isRecording: isRecording,
            isTranscribing: isTranscribing
        )
        guard startAction != .reject else {
            onStarted(false)
            return
        }
        cancelAmbientMetering()
        shouldArchiveCurrentCapture = false
        voiceNoteCapturedAt = nil
        ambientCaptureGeneration += 1
        let generation = ambientCaptureGeneration
        ambientStartCompletion = onStarted
        ambientSpeechBegan = onSpeechBegan
        ambientResultCompletion = onResult
        partialTranscript = ""
        if startAction == .resumeRetainedInputGraph {
            resumeAmbientCaptureEngine(generation: generation)
        } else {
            requestAndStart(live: false, ambientGeneration: generation)
        }
    }

    nonisolated static func ambientStartAction(
        captureRoute: DictationCaptureRoute,
        retainedInputGraph: Bool,
        isRecording: Bool,
        isTranscribing: Bool
    ) -> AmbientDictationStartAction {
        guard !isRecording, !isTranscribing else { return .reject }
        if captureRoute == .file, retainedInputGraph {
            return .resumeRetainedInputGraph
        }
        return captureRoute == .idle ? .createInputGraph : .reject
    }

    /// Cancel is silent: the ambient controller already knows it requested the
    /// teardown, so reporting a result would incorrectly re-arm the microphone.
    @MainActor
    func cancelAmbientCapture() {
        ambientCaptureGeneration += 1
        let pendingStart = ambientStartCompletion
        ambientStartCompletion = nil
        ambientSpeechBegan = nil
        ambientResultCompletion = nil
        ambientInputReadyWork?.cancel()
        ambientInputReadyWork = nil
        cancelAmbientMetering()
        cancelFinishFallback()
        ambientMic?.stop()
        ambientMic = nil
        ambientPCM.reset()
        recorder?.stop()
        recorder = nil
        if let url = fileURL { try? FileManager.default.removeItem(at: url) }
        fileURL = nil
        if let engine = audioEngine {
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
        }
        audioEngine = nil
        liveRequest?.endAudio()
        liveRequest = nil
        liveTask?.cancel()
        liveTask = nil
        onDeviceTranscriptionTask?.cancel()
        onDeviceTranscriptionTask = nil
        cloudTranscriptionTask?.cancel()
        cloudTranscriptionTask = nil
        if let url = transcriptionLease.cancel() {
            try? FileManager.default.removeItem(at: url)
        }
        transcriptionCompletion = nil
        finishCompletion = nil
        captureRoute = .idle
        isRecording = false
        isTranscribing = false
        partialTranscript = ""
        releaseSession()
        pendingStart?(false)
    }

    /// Hold-to-talk ends. Delivers the final transcript on the main queue.
    func finishLive(completion: @escaping (String?) -> Void) {
        switch captureRoute.finishAction {
        case .finalizeLive:
            finishLiveStreaming(completion)
        case .transcribeFile:
            stopAndTranscribe(completion: completion)
        case .none:
            completion(nil)
        }
    }

    private func startLiveStreaming(ambientGeneration: Int? = nil) {
        guard acceptsAmbientGeneration(ambientGeneration) else { return }
        guard let recognizer = recognizer, recognizer.isAvailable else {
            beginRecording(ambientGeneration: ambientGeneration)
            return
        }
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.playAndRecord, mode: .spokenAudio, options: [.defaultToSpeaker, .allowBluetoothHFP])
            try session.setActive(true)
        } catch {
            debugLog("[dictation] live session error: \(error)")
            beginRecording(ambientGeneration: ambientGeneration)
            return
        }

        let engine = AVAudioEngine()
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.requiresOnDeviceRecognition = true
        let node = engine.inputNode
        node.installTap(onBus: 0, bufferSize: 1024, format: node.outputFormat(forBus: 0)) { buffer, _ in
            request.append(buffer)
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            node.removeTap(onBus: 0)
            request.endAudio()
            debugLog("[dictation] engine error: \(error)")
            beginRecording(ambientGeneration: ambientGeneration)
            return
        }

        liveTask = recognizer.recognitionTask(with: request) { [weak self] result, _ in
            guard let self = self, let result = result else { return }
            let text = result.bestTranscription.formattedString
            DispatchQueue.main.async { self.partialTranscript = text }
            if result.isFinal { DispatchQueue.main.async { self.deliverFinal(text) } }
        }
        audioEngine = engine
        liveRequest = request
        captureRoute = .live
        isRecording = true
        resolveAmbientStart(true, generation: ambientGeneration)
    }

    private func finishLiveStreaming(_ completion: @escaping (String?) -> Void) {
        finishCompletion = completion
        captureRoute = .idle
        audioEngine?.inputNode.removeTap(onBus: 0)
        audioEngine?.stop()
        liveRequest?.endAudio()
        isRecording = false
        isTranscribing = true
        releaseSession()
        let work = DispatchWorkItem { [weak self] in self?.deliverFinal(self?.partialTranscript ?? "") }
        finishFallbackWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2, execute: work)
    }

    private func deliverFinal(_ text: String) {
        guard let completion = finishCompletion else { return }   // one-shot
        finishCompletion = nil
        cancelFinishFallback()
        liveTask?.cancel(); liveTask = nil
        audioEngine = nil
        liveRequest = nil
        isTranscribing = false
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        completion(trimmed.isEmpty ? nil : trimmed)
    }

    private func cancelFinishFallback() { finishFallbackWork?.cancel(); finishFallbackWork = nil }

    private func acceptsAmbientGeneration(_ generation: Int?) -> Bool {
        guard let generation else { return true }
        return generation == ambientCaptureGeneration && ambientResultCompletion != nil
    }

    private func resolveAmbientStart(_ started: Bool, generation: Int?) {
        guard let generation, generation == ambientCaptureGeneration else { return }
        ambientInputReadyWork?.cancel()
        ambientInputReadyWork = nil
        let completion = ambientStartCompletion
        ambientStartCompletion = nil
        completion?(started)
        if !started {
            ambientSpeechBegan = nil
            ambientResultCompletion = nil
        }
    }

    private func beginAmbientMetering(generation: Int) {
        cancelAmbientMetering()
        ambientSilenceGate = AmbientDictationSilenceGate(
            startedAt: ProcessInfo.processInfo.systemUptime
        )
        let timer = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in
            guard let self else { return }
            MainActor.assumeIsolated {
                guard self.acceptsAmbientGeneration(generation),
                      var gate = self.ambientSilenceGate else { return }
                let event = gate.observe(
                    powerDB: self.ambientPCM.averagePowerDB,
                    at: ProcessInfo.processInfo.systemUptime
                )
                self.ambientSilenceGate = gate
                self.applyAmbientMeterEvent(event, generation: generation)
            }
        }
        ambientMeterTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    @MainActor
    private func applyAmbientMeterEvent(
        _ event: AmbientDictationSilenceGate.Event,
        generation: Int
    ) {
        guard acceptsAmbientGeneration(generation) else { return }
        switch event {
        case .none:
            break
        case .speechBegan:
            debugLog("[dictation] ambient speech began generation=\(generation)")
            let callback = ambientSpeechBegan
            ambientSpeechBegan = nil
            callback?()
        case .finishUtterance:
            cancelAmbientMetering()
            pauseAndTranscribeAmbient(generation: generation)
        case .noSpeech:
            debugLog(
                "[dictation] ambient follow-up expired without speech generation=\(generation) buffered_bytes=\(ambientPCM.bufferedByteCount)"
            )
            let completion = ambientResultCompletion
            ambientResultCompletion = nil
            ambientSpeechBegan = nil
            cancelAmbientMetering()
            teardownRetainedAmbientCapture()
            releaseSession()
            completion?(.noSpeech)
        }
    }

    /// Close only the in-memory turn gate, leaving the background-admitted input
    /// graph running. The completed PCM16 bytes become a fully finalised WAV
    /// before STT sees them; unlike the old paused-recorder file, this asset has
    /// no mutable header or writer still attached to it.
    @MainActor
    private func pauseAndTranscribeAmbient(generation: Int) {
        guard acceptsAmbientGeneration(generation),
              ambientMic != nil,
              captureRoute == .file,
              let snapshot = ambientPCM.finishTurn() else {
            debugLog("[dictation] ambient utterance ended without buffered PCM generation=\(generation)")
            completeAmbientCapture(.failedToTranscribe, generation: generation)
            return
        }

        isRecording = false
        debugLog(
            "[dictation] ambient utterance buffered generation=\(generation) bytes=\(snapshot.pcm16LE.count)"
        )
        let segmentURL: URL
        do {
            segmentURL = FileManager.default.temporaryDirectory.appendingPathComponent(
                "ambient-dictation-segment-\(UUID().uuidString).wav"
            )
            try snapshot.wavData().write(to: segmentURL, options: .atomic)
        } catch {
            debugLog("[dictation] ambient segment snapshot failed: \(error.localizedDescription)")
            teardownRetainedAmbientCapture()
            releaseSession()
            completeAmbientCapture(.failedToTranscribe, generation: generation)
            return
        }

        isTranscribing = true
        let transcriptionGeneration = transcriptionLease.begin(fileURL: segmentURL)
        transcriptionCompletion = { [weak self] transcript in
            MainActor.assumeIsolated {
                debugLog(
                    "[dictation] ambient STT completed generation=\(generation) transcript_chars=\(transcript?.count ?? 0)"
                )
                self?.completeAmbientCapture(
                    transcript.map(AmbientDictationCaptureResult.transcript)
                        ?? .failedToTranscribe,
                    generation: generation
                )
            }
        }
        beginFileTranscription(url: segmentURL, generation: transcriptionGeneration)
    }

    @MainActor
    private func teardownRetainedAmbientCapture() {
        ambientInputReadyWork?.cancel()
        ambientInputReadyWork = nil
        ambientMic?.stop()
        ambientMic = nil
        ambientPCM.reset()
        captureRoute = .idle
        isRecording = false
    }

    private func completeAmbientCapture(
        _ result: AmbientDictationCaptureResult,
        generation: Int
    ) {
        guard generation == ambientCaptureGeneration else { return }
        let completion = ambientResultCompletion
        ambientResultCompletion = nil
        ambientSpeechBegan = nil
        completion?(result)
    }

    private func cancelAmbientMetering() {
        ambientMeterTimer?.invalidate()
        ambientMeterTimer = nil
        ambientSilenceGate = nil
    }

    /// Give the shared audio session back — **unless an ambient window is armed.**
    ///
    /// The one place this file deactivates, collapsed from the three it used to.
    /// That is the substance of the rail rather than tidiness: design §15 records
    /// that its own landmine table was wrong twice, and the lesson it draws is that
    /// *"a deactivation is only safe if something structurally prevents the call,
    /// not if a survey once concluded the path was unreachable."* One guarded exit
    /// is that structure. Three copies of `setActive(false, …)` were not, and a
    /// fourth added later would not have been either.
    ///
    /// It is a BACKSTOP, not the primary rail: `startLive` yields the window before
    /// any of this runs, so in the ordinary flow there is nothing armed by the time
    /// we get here. It exists for the flow that skips `startLive` — a future entry
    /// point, or a capture the app inherits — where the cost of being wrong is a
    /// session that cannot be reactivated from the background and an armed window
    /// that dies silently at the next wake word (Apple DTS 826462).
    ///
    /// `MainActor.assumeIsolated` rather than an `@MainActor` annotation because
    /// this is reached from the speech- and recorder-permission callbacks, which
    /// are already hopped onto the main queue with `DispatchQueue.main.async` and
    /// cannot call main-actor-isolated code without one.
    ///
    /// `internal` and counted, for the same reason `VoiceAudioEngine.releaseSessionIfRequested`
    /// and `SpeechSynthesizer.sessionReleaseCount` are: **"it did not deactivate" is
    /// not observable through any `AVAudioSession` API**, so a private method with no
    /// counter is a structural backstop that nothing can prove exists. This is the
    /// highest-traffic rail in the set — every dictation start and every dictation
    /// finish reaches it — and it was the only one with no coverage at all.
    func releaseSession() {
        let ambientWindowIsLive = MainActor.assumeIsolated { ambientRail.windowIsLive() }
        guard !ambientWindowIsLive else {
            debugLog("[dictation] leaving the audio session active: an ambient window is armed")
            return
        }
        sessionReleaseCount += 1
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    private var onDeviceAvailable: Bool {
        guard let recognizer = recognizer, recognizer.isAvailable,
              recognizer.supportsOnDeviceRecognition,
              SFSpeechRecognizer.authorizationStatus() == .authorized else { return false }
        return true
    }

    /// Transcribe the recorded file with Apple's on-device recognizer — free,
    /// offline, private. Returns nil on any error so the caller can fall back.
    private func transcribeOnDevice(
        _ url: URL,
        generation: Int,
        allowsCloudFallback: Bool
    ) {
        guard let recognizer = recognizer else {
            handleOnDeviceTranscription(
                nil,
                generation: generation,
                allowsCloudFallback: allowsCloudFallback
            )
            return
        }
        let request = SFSpeechURLRecognitionRequest(url: url)
        request.requiresOnDeviceRecognition = true
        onDeviceTranscriptionTask = recognizer.recognitionTask(with: request) {
            [weak self] result, error in
            if let result = result, result.isFinal {
                let transcript = result.bestTranscription.formattedString
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                DispatchQueue.main.async {
                    self?.handleOnDeviceTranscription(
                        transcript,
                        generation: generation,
                        allowsCloudFallback: allowsCloudFallback
                    )
                }
            } else if error != nil {
                DispatchQueue.main.async {
                    self?.handleOnDeviceTranscription(
                        nil,
                        generation: generation,
                        allowsCloudFallback: allowsCloudFallback
                    )
                }
            }
        }
    }

    private func handleOnDeviceTranscription(
        _ transcript: String?,
        generation: Int,
        allowsCloudFallback: Bool
    ) {
        guard transcriptionLease.claimOnDeviceResult(generation) else { return }
        onDeviceTranscriptionTask = nil
        if let transcript, !transcript.isEmpty {
            finishTranscription(transcript, generation: generation)
        } else if allowsCloudFallback {
            beginCloudTranscription(generation: generation)
        } else {
            finishTranscription(nil, generation: generation)
        }
    }

    private func beginCloudTranscription(generation: Int) {
        guard let url = transcriptionLease.beginCloud(generation) else { return }
        let upload = DictationAudioUpload.file(at: url)
        let data = try? Data(contentsOf: url)
        // The in-memory request body owns successful reads from here. Failed
        // reads still relinquish the lease's file; otherwise an unreadable temp
        // recording survives forever because `beginCloud` already moved state.
        try? FileManager.default.removeItem(at: url)
        guard let data, !data.isEmpty else {
            finishTranscription(nil, generation: generation)
            return
        }
        transcribeCloud(data, upload: upload, generation: generation)
    }

    private func transcribeCloud(
        _ data: Data,
        upload: DictationAudioUpload,
        generation: Int
    ) {
        guard transcriptionLease.isCloudActive(generation) else { return }
        var components = URLComponents(
            string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/media/stt/transcribe"
        )
        var queryItems: [URLQueryItem] = []
        let audio = AudioSettings.shared
        if let profile = audio.requestProfile(for: .dictation) {
            queryItems.append(URLQueryItem(name: "profile", value: profile))
        }
        if let option = audio.requestStageOptions(for: .dictation)[NativeAudioStage.recordingSTT.rawValue] {
            queryItems.append(URLQueryItem(
                name: "stage_option",
                value: "\(NativeAudioStage.recordingSTT.rawValue):\(option)"
            ))
        }
        components?.queryItems = queryItems
        guard let url = components?.url else {
            finishTranscription(nil, generation: generation)
            return
        }
        let boundary = "Boundary-\(UUID().uuidString)"
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        var body = Data()
        body.append("--\(boundary)\r\n".data(using: .utf8)!)
        body.append("Content-Disposition: form-data; name=\"file\"; filename=\"\(upload.filename)\"\r\n".data(using: .utf8)!)
        body.append("Content-Type: \(upload.contentType)\r\n\r\n".data(using: .utf8)!)
        body.append(data)
        body.append("\r\n--\(boundary)--\r\n".data(using: .utf8)!)
        request.httpBody = body
        let task = URLSession.shared.dataTask(with: request) { [weak self] data, _, _ in
            var transcript: String?
            if let data = data, let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                transcript = (obj["transcript"] as? String) ?? (obj["text"] as? String)
            }
            DispatchQueue.main.async {
                self?.finishTranscription(
                    transcript?.trimmingCharacters(in: .whitespacesAndNewlines),
                    generation: generation
                )
            }
        }
        cloudTranscriptionTask = task
        task.resume()
    }

    private func finishTranscription(_ transcript: String?, generation: Int) {
        guard transcriptionLease.accepts(generation) else { return }
        let remainingURL = transcriptionLease.finish(generation)
        if let remainingURL {
            try? FileManager.default.removeItem(at: remainingURL)
        }
        onDeviceTranscriptionTask = nil
        cloudTranscriptionTask = nil
        let completion = transcriptionCompletion
        transcriptionCompletion = nil
        if pendingAudioNote?.generation == generation, let audioNote = pendingAudioNote {
            pendingAudioNote = nil
            audioNoteUploadQueue.markReady(id: audioNote.id, transcript: transcript)
        }
        isTranscribing = false
        completion?(transcript)
    }
}

/// `requestRecordPermission` is a `@Sendable` system callback while this
/// controller is main-thread owned. The unchecked wrapper carries only a weak
/// reference and is dereferenced after the explicit hop to the main queue; it
/// avoids falsely declaring the mutable controller itself `Sendable`.
private final class WeakDictationController: @unchecked Sendable {
    weak var value: DictationController?

    init(_ value: DictationController) {
        self.value = value
    }
}

/// Disk-backed handoff between explicit iOS voice-note capture and the Notes
/// provider. A note leaves this outbox only after the server has durably written
/// both the audio and its Markdown page. Interrupted STT is recovered as an
/// audio-only note on the next launch instead of silently losing the recording.
struct AudioNoteOutboxRecord: Codable, Equatable {
    let id: String
    let capturedAt: String
    let durationMS: Int?
    let sourceSurface: String
    let audioFilename: String
    var transcript: String?
    var ready: Bool
    var destinationBaseURL: String? = nil
    var principal: String? = nil
    var workspace: String? = nil
    /// `nil` means the scoped default provider. Kept optional so old queued
    /// records migrate and future explicit provider choices remain possible.
    var provider: String? = nil
    var attemptCount: Int? = nil
    var nextAttemptAt: Date? = nil
    var failedPermanently: Bool? = nil
    var lastError: String? = nil
}

struct AudioNoteUploadReceipt: Codable, Equatable {
    let noteID: String
    let provider: String
    let capturedAt: String
    let notePath: String
    let audioPath: String
    let bytes: Int

    enum CodingKeys: String, CodingKey {
        case noteID = "note_id"
        case provider
        case capturedAt = "captured_at"
        case notePath = "note_path"
        case audioPath = "audio_path"
        case bytes
    }
}

enum AudioNoteUploadOutcome: Equatable {
    case success(AudioNoteUploadReceipt)
    case retry(String)
    case permanentFailure(String)
}

struct AudioNoteOutboxStatus: Identifiable, Equatable {
    let id: String
    let capturedAt: String
    let transcript: String?
    let state: String
    let detail: String?
    let canDiscard: Bool
}

struct AudioNoteRestoredTaskPlan: Equatable {
    let acceptedTaskIDs: Set<Int>
    let acceptedRecordIDs: Set<String>
    let canceledTaskIDs: Set<Int>
}

enum AudioNoteOutboxError: LocalizedError {
    case emptyRecording
    case recordingTooLarge
    case outboxFull
    case stagingFailed(String)

    var errorDescription: String? {
        switch self {
        case .emptyRecording:
            return "The recording was empty, so no Audio Note was created."
        case .recordingTooLarge:
            return "The recording is larger than the 24 MB Audio Note limit. Shorten it and try again."
        case .outboxFull:
            return "The Audio Note outbox is full. Open Audio Notes to retry or discard failed uploads."
        case .stagingFailed(let message):
            return "The recording could not be protected in the Audio Note outbox: \(message)"
        }
    }
}

/// Protected, bounded, file-backed upload queue. Each record advances
/// independently, so one permanent failure never arrests later notes. Uploads
/// are background-session file tasks and are deleted only after a matching,
/// structurally valid server receipt arrives.
final class AudioNoteUploadQueue: NSObject, ObservableObject, URLSessionDataDelegate, URLSessionTaskDelegate {
    static let shared = AudioNoteUploadQueue()

    static let maximumRecordingBytes: Int64 = 24 * 1_024 * 1_024
    static let maximumOutboxBytes: Int64 = 200 * 1_024 * 1_024
    static let maximumOutboxRecords = 50
    static let maximumAutomaticRetryAge: TimeInterval = 30 * 24 * 60 * 60
    static let maximumAttempts = 10
    static let maximumTranscriptBytes = 12 * 1_024
    static let maximumPerRecordAuxiliaryBytes: Int64 = 32 * 1_024
    static let maximumBackgroundTasks = 4
    static let backgroundSessionIdentifier = "ai.magicbeans.magican.audio-notes"
    static let fileProtectionType = FileProtectionType.complete

    @Published private(set) var statuses: [AudioNoteOutboxStatus] = []

    private let fileManager: FileManager
    private let directory: URL
    private let worker = DispatchQueue(label: "com.magican.magios.audio-note-outbox")
    private let networkingEnabled: Bool
    private var activeTaskIDs: Set<Int> = []
    private var activeRecordIDs: Set<String> = []
    private var restorationStarted = false
    private var restoredBackgroundTasks = false
    private var responseBodies: [Int: Data] = [:]
    private let backgroundCompletionLock = NSLock()
    private var backgroundCompletionHandler: (() -> Void)?

    private lazy var session: URLSession = {
        let configuration = URLSessionConfiguration.background(
            withIdentifier: Self.backgroundSessionIdentifier
        )
        configuration.sessionSendsLaunchEvents = true
        configuration.isDiscretionary = false
        configuration.waitsForConnectivity = true
        configuration.timeoutIntervalForRequest = 90
        configuration.timeoutIntervalForResource = 15 * 60
        return URLSession(configuration: configuration, delegate: self, delegateQueue: nil)
    }()

    init(
        fileManager: FileManager = .default,
        directory: URL? = nil,
        networkingEnabled: Bool = !isRunningUnderTests
    ) {
        self.fileManager = fileManager
        self.networkingEnabled = networkingEnabled
        let applicationSupport = fileManager.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first ?? fileManager.temporaryDirectory
        self.directory = directory ?? applicationSupport
            .appendingPathComponent("Magican", isDirectory: true)
            .appendingPathComponent("Audio Note Outbox", isDirectory: true)
        super.init()
        secureDirectory()
        guard networkingEnabled else {
            restorationStarted = true
            restoredBackgroundTasks = true
            return
        }
        recoverInterruptedCaptures()
    }

    /// Copy off the main actor, then invoke completion on the main queue. The
    /// caller starts transcription only after this handoff completes, so the
    /// temporary recorder file cannot be removed before the protected copy exists.
    func stageRecording(
        at recordingURL: URL,
        capturedAt: Date,
        durationMS: Int?,
        completion: @escaping (Result<String, Error>) -> Void
    ) {
        let id = UUID().uuidString.lowercased()
        let destinationBaseURL = MagicianAccess.baseURL.absoluteString
        let capturedPrincipal = MagicianAccess.principal
        let capturedWorkspace = MagicianAccess.workspace
        worker.async { [weak self] in
            guard let self else { return }
            let result: Result<String, Error>
            do {
                let values = try recordingURL.resourceValues(forKeys: [.fileSizeKey])
                let byteCount = Int64(values.fileSize ?? 0)
                guard byteCount > 0 else { throw AudioNoteOutboxError.emptyRecording }
                guard byteCount <= Self.maximumRecordingBytes else {
                    throw AudioNoteOutboxError.recordingTooLarge
                }
                try self.ensureCapacity(for: byteCount)
                let filename = "\(id).m4a"
                let audioURL = self.directory.appendingPathComponent(filename)
                let record = AudioNoteOutboxRecord(
                    id: id,
                    capturedAt: Self.timestamp(capturedAt),
                    durationMS: durationMS,
                    sourceSurface: "ios_voice_note",
                    audioFilename: filename,
                    transcript: nil,
                    ready: false,
                    destinationBaseURL: destinationBaseURL,
                    principal: capturedPrincipal,
                    workspace: capturedWorkspace,
                    provider: nil,
                    attemptCount: 0,
                    nextAttemptAt: nil,
                    failedPermanently: false,
                    lastError: nil
                )
                var copiedRecording = false
                do {
                    try self.fileManager.copyItem(at: recordingURL, to: audioURL)
                    copiedRecording = true
                    try self.protectFile(audioURL)
                    try self.persist(record)
                } catch {
                    if copiedRecording {
                        try? self.fileManager.removeItem(at: audioURL)
                    }
                    throw AudioNoteOutboxError.stagingFailed(error.localizedDescription)
                }
                self.publishStatuses()
                result = .success(id)
            } catch {
                result = .failure(error)
                debugLog("[audio-note] failed to stage recording: \(error.localizedDescription)")
            }
            DispatchQueue.main.async { completion(result) }
        }
    }

    func markReady(id: String, transcript: String?) {
        worker.async { [weak self] in
            guard let self, var record = self.loadRecord(id: id) else { return }
            record.transcript = Self.boundedTranscript(transcript)
            record.ready = true
            record.lastError = nil
            do {
                try self.persist(record)
            } catch {
                debugLog("[audio-note] failed to finalize outbox metadata: \(error.localizedDescription)")
                return
            }
            self.publishStatuses()
            self.drain()
        }
    }

    func resume() {
        worker.async { [weak self] in self?.drain() }
    }

    func retry(id: String) {
        worker.async { [weak self] in
            guard let self, var record = self.loadRecord(id: id) else { return }
            record.attemptCount = 0
            record.nextAttemptAt = nil
            record.failedPermanently = false
            record.lastError = nil
            try? self.persist(record)
            self.publishStatuses()
            self.drain()
        }
    }

    func discard(id: String) {
        worker.async { [weak self] in
            guard let self, let record = self.loadRecord(id: id) else { return }
            guard !self.activeRecordIDs.contains(id) else { return }
            self.remove(record)
            self.publishStatuses()
            self.drain()
        }
    }

    func loadRecording(id: String, completion: @escaping (Result<Data, Error>) -> Void) {
        worker.async { [weak self] in
            guard let self, let record = self.loadRecord(id: id) else {
                DispatchQueue.main.async {
                    completion(.failure(URLError(.fileDoesNotExist)))
                }
                return
            }
            let result = Result {
                try Data(contentsOf: self.directory.appendingPathComponent(record.audioFilename))
            }
            DispatchQueue.main.async { completion(result) }
        }
    }

    func setBackgroundCompletionHandler(_ completion: @escaping () -> Void) {
        // Install UIKit's handler before the lazy session is ever reconnected.
        // A background-only relaunch reaches this method before the normal
        // foreground `resume()` path.
        backgroundCompletionLock.lock()
        backgroundCompletionHandler = completion
        backgroundCompletionLock.unlock()
        worker.async { [weak self] in self?.drain() }
    }

    /// Pure in-memory encoder retained for contract coverage. Production uses
    /// `writeMultipartFile` so a 24 MB recording never becomes two or three
    /// additional in-memory copies.
    static func multipartBody(
        record: AudioNoteOutboxRecord,
        audio: Data,
        boundary: String
    ) -> Data {
        var body = multipartHeader(record: record, boundary: boundary)
        body.append(audio)
        append("\r\n--\(boundary)--\r\n", to: &body)
        return body
    }

    static func uploadOutcome(
        recordID: String,
        expectedCapturedAt: String,
        expectedBytes: Int,
        data: Data,
        status: Int?,
        error: Error?
    ) -> AudioNoteUploadOutcome {
        if let error {
            return .retry(error.localizedDescription)
        }
        guard let status else { return .retry("The server returned no HTTP status.") }
        if (200..<300).contains(status) {
            guard let receipt = try? JSONDecoder().decode(AudioNoteUploadReceipt.self, from: data),
                  receipt.noteID.caseInsensitiveCompare(recordID) == .orderedSame,
                  captureTimestampsMatch(receipt.capturedAt, expectedCapturedAt),
                  receipt.bytes == expectedBytes,
                  !receipt.provider.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  !receipt.notePath.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  !receipt.audioPath.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                return .retry("The server did not return a matching durable Audio Note receipt.")
            }
            return .success(receipt)
        }
        if status == 408 || status == 425 || status == 429 || (500..<600).contains(status) {
            return .retry("The Audio Notes service returned HTTP \(status).")
        }
        return .permanentFailure("Upload was rejected with HTTP \(status).")
    }

    static func nextEligibleRecord(
        from records: [AudioNoteOutboxRecord],
        now: Date
    ) -> AudioNoteOutboxRecord? {
        records
            .filter {
                $0.ready
                    && $0.failedPermanently != true
                    && ($0.nextAttemptAt ?? .distantPast) <= now
            }
            .sorted {
                (captureDate($0.capturedAt) ?? .distantPast)
                    < (captureDate($1.capturedAt) ?? .distantPast)
            }
            .first
    }

    static func outboxStatus(
        for record: AudioNoteOutboxRecord,
        active: Bool,
        now: Date = Date()
    ) -> AudioNoteOutboxStatus {
        let retryScheduled = active && (record.nextAttemptAt ?? .distantPast) > now
        return AudioNoteOutboxStatus(
            id: record.id,
            capturedAt: record.capturedAt,
            transcript: record.transcript,
            state: retryScheduled
                ? "Retry scheduled"
                : (active
                ? "Uploading"
                : (record.failedPermanently == true
                ? "Needs attention"
                : (record.ready ? "Waiting to upload" : "Preparing"))),
            detail: record.lastError,
            canDiscard: !active
        )
    }

    private func restoreBackgroundTasks() {
        session.getAllTasks { [weak self] tasks in
            self?.worker.async {
                guard let self else { return }
                let validRecordIDs = Set(self.records().map(\.id))
                let plan = Self.restoredTaskPlan(
                    tasks.map {
                        (taskID: $0.taskIdentifier, recordID: $0.taskDescription)
                    },
                    validRecordIDs: validRecordIDs
                )
                self.activeTaskIDs = plan.acceptedTaskIDs
                self.activeRecordIDs = plan.acceptedRecordIDs
                for task in tasks where plan.canceledTaskIDs.contains(task.taskIdentifier) {
                    task.cancel()
                }
                self.restoredBackgroundTasks = true
                self.publishStatuses()
                self.drain()
            }
        }
    }

    /// Returns true when this call started the asynchronous restoration pass.
    /// Must run on `worker`; all callers already serialize through that queue.
    @discardableResult
    private func beginSessionRestorationIfNeeded() -> Bool {
        guard networkingEnabled, !restorationStarted else { return false }
        restorationStarted = true
        restoreBackgroundTasks()
        return true
    }

    static func restoredTaskPlan(
        _ tasks: [(taskID: Int, recordID: String?)],
        validRecordIDs: Set<String>
    ) -> AudioNoteRestoredTaskPlan {
        var acceptedTaskIDs = Set<Int>()
        var acceptedRecordIDs = Set<String>()
        var canceledTaskIDs = Set<Int>()
        for task in tasks {
            guard let recordID = task.recordID,
                  UUID(uuidString: recordID) != nil,
                  validRecordIDs.contains(recordID),
                  acceptedRecordIDs.insert(recordID).inserted else {
                canceledTaskIDs.insert(task.taskID)
                continue
            }
            acceptedTaskIDs.insert(task.taskID)
        }
        return AudioNoteRestoredTaskPlan(
            acceptedTaskIDs: acceptedTaskIDs,
            acceptedRecordIDs: acceptedRecordIDs,
            canceledTaskIDs: canceledTaskIDs
        )
    }

    private func recoverInterruptedCaptures() {
        worker.async { [weak self] in
            guard let self else { return }
            for record in self.records() where !record.ready {
                var recovered = record
                recovered.ready = true
                recovered.lastError = "Recovered after interruption; transcript was unavailable."
                try? self.persist(recovered)
            }
            self.publishStatuses()
        }
    }

    private func drain() {
        if beginSessionRestorationIfNeeded() { return }
        guard networkingEnabled,
              restoredBackgroundTasks,
              activeTaskIDs.count == activeRecordIDs.count,
              activeTaskIDs.count < Self.maximumBackgroundTasks else { return }
        let now = Date()
        var candidates = records().filter {
            $0.ready
                && $0.failedPermanently != true
                && !activeRecordIDs.contains($0.id)
        }
        for index in candidates.indices {
            if Self.captureDate(candidates[index].capturedAt).map({ now.timeIntervalSince($0) }) ?? 0
                > Self.maximumAutomaticRetryAge {
                var expired = candidates[index]
                expired.failedPermanently = true
                expired.lastError = "Automatic retries stopped after 30 days. Retry or discard this local copy."
                try? persist(expired)
                candidates[index] = expired
            }
        }
        candidates = candidates.filter { $0.failedPermanently != true }
        candidates.sort {
            (Self.captureDate($0.capturedAt) ?? .distantPast)
                < (Self.captureDate($1.capturedAt) ?? .distantPast)
        }
        let eligible = Self.nextEligibleRecord(from: candidates, now: now)
        let activeFutureRetryExists = activeRecordIDs.compactMap { id in
            loadRecord(id: id)
        }.contains {
            ($0.nextAttemptAt ?? .distantPast) > now
        }
        let future = activeFutureRetryExists ? nil : candidates
            .filter { ($0.nextAttemptAt ?? .distantPast) > now }
            .min { ($0.nextAttemptAt ?? .distantFuture) < ($1.nextAttemptAt ?? .distantFuture) }
        guard let record = eligible ?? future else {
            publishStatuses()
            return
        }

        let audioURL = directory.appendingPathComponent(record.audioFilename)
        guard let expectedBytes = try? audioURL.resourceValues(forKeys: [.fileSizeKey]).fileSize,
              expectedBytes > 0 else {
            if markPermanentFailure(record, message: "The protected recording is missing or empty.") {
                worker.async { [weak self] in self?.drain() }
            }
            return
        }

        let boundary = "Boundary-\(UUID().uuidString)"
        let multipartURL = directory.appendingPathComponent("\(record.id).upload")
        do {
            try writeMultipartFile(
                record: record,
                audioURL: audioURL,
                destination: multipartURL,
                boundary: boundary
            )
        } catch {
            try? fileManager.removeItem(at: multipartURL)
            if markPermanentFailure(
                record,
                message: "Could not prepare the protected upload body: \(error.localizedDescription)"
            ) {
                worker.async { [weak self] in self?.drain() }
            }
            return
        }

        guard let baseURL = Self.validatedDestinationBaseURL(record.destinationBaseURL) else {
            try? fileManager.removeItem(at: multipartURL)
            if markPermanentFailure(record, message: "The saved Audio Note destination is not trusted.") {
                worker.async { [weak self] in self?.drain() }
            }
            return
        }
        let components = URLComponents(
            url: baseURL.appendingPathComponent("/api/magician/v2/notes/audio"),
            resolvingAgainstBaseURL: false
        )
        guard let url = components?.url else {
            try? fileManager.removeItem(at: multipartURL)
            if markPermanentFailure(record, message: "The saved Audio Note destination is invalid.") {
                worker.async { [weak self] in self?.drain() }
            }
            return
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)

        let task = session.uploadTask(with: request, fromFile: multipartURL)
        task.taskDescription = record.id
        if let nextAttemptAt = record.nextAttemptAt, nextAttemptAt > now {
            task.earliestBeginDate = nextAttemptAt
        }
        responseBodies[task.taskIdentifier] = Data()
        activeTaskIDs.insert(task.taskIdentifier)
        activeRecordIDs.insert(record.id)
        publishStatuses()
        task.resume()
        worker.async { [weak self] in self?.drain() }
    }

    @discardableResult
    private func markRetry(_ record: AudioNoteOutboxRecord, message: String) -> Bool {
        var updated = record
        let attempt = (updated.attemptCount ?? 0) + 1
        updated.attemptCount = attempt
        updated.lastError = message
        if attempt >= Self.maximumAttempts {
            updated.failedPermanently = true
            updated.nextAttemptAt = nil
            updated.lastError = "Automatic retries stopped after \(attempt) attempts. \(message)"
        } else {
            let delays: [TimeInterval] = [15, 30, 60, 120, 300, 600, 900]
            updated.nextAttemptAt = Date().addingTimeInterval(delays[min(attempt - 1, delays.count - 1)])
        }
        do {
            try persist(updated)
        } catch {
            debugLog("[audio-note] could not persist retry state: \(error.localizedDescription)")
            return false
        }
        publishStatuses()
        return true
    }

    @discardableResult
    private func markPermanentFailure(_ record: AudioNoteOutboxRecord, message: String) -> Bool {
        var updated = record
        updated.failedPermanently = true
        updated.nextAttemptAt = nil
        updated.lastError = message
        do {
            try persist(updated)
        } catch {
            debugLog("[audio-note] could not persist terminal state: \(error.localizedDescription)")
            return false
        }
        publishStatuses()
        return true
    }

    private func writeMultipartFile(
        record: AudioNoteOutboxRecord,
        audioURL: URL,
        destination: URL,
        boundary: String
    ) throws {
        try? fileManager.removeItem(at: destination)
        guard fileManager.createFile(atPath: destination.path, contents: nil) else {
            throw AudioNoteOutboxError.stagingFailed("could not create upload body")
        }
        let output = try FileHandle(forWritingTo: destination)
        defer { try? output.close() }
        try output.write(contentsOf: Self.multipartHeader(record: record, boundary: boundary))
        let input = try FileHandle(forReadingFrom: audioURL)
        defer { try? input.close() }
        while let chunk = try input.read(upToCount: 64 * 1_024), !chunk.isEmpty {
            try output.write(contentsOf: chunk)
        }
        try output.write(contentsOf: Data("\r\n--\(boundary)--\r\n".utf8))
        try output.synchronize()
        try protectFile(destination)
    }

    private static func multipartHeader(record: AudioNoteOutboxRecord, boundary: String) -> Data {
        var body = Data()
        if let provider = record.provider?.trimmingCharacters(in: .whitespacesAndNewlines),
           !provider.isEmpty {
            appendField("provider", value: provider, boundary: boundary, to: &body)
        }
        appendField("note_id", value: record.id, boundary: boundary, to: &body)
        appendField("captured_at", value: record.capturedAt, boundary: boundary, to: &body)
        appendField("source_surface", value: record.sourceSurface, boundary: boundary, to: &body)
        if let durationMS = record.durationMS {
            appendField("duration_ms", value: String(durationMS), boundary: boundary, to: &body)
        }
        if let transcript = boundedTranscript(record.transcript) {
            appendField("transcript", value: transcript, boundary: boundary, to: &body)
        }
        append("--\(boundary)\r\n", to: &body)
        append("Content-Disposition: form-data; name=\"file\"; filename=\"voice-note.m4a\"\r\n", to: &body)
        append("Content-Type: audio/m4a\r\n\r\n", to: &body)
        return body
    }

    private static func boundedTranscript(_ transcript: String?) -> String? {
        guard let trimmed = transcript?.trimmingCharacters(in: .whitespacesAndNewlines),
              !trimmed.isEmpty else { return nil }
        var data = Data(trimmed.utf8.prefix(maximumTranscriptBytes))
        while !data.isEmpty, String(data: data, encoding: .utf8) == nil {
            data.removeLast()
        }
        return String(data: data, encoding: .utf8)
    }

    private func ensureCapacity(for incomingBytes: Int64) throws {
        let currentRecords = records()
        guard currentRecords.count < Self.maximumOutboxRecords else {
            throw AudioNoteOutboxError.outboxFull
        }
        let urls = (try? fileManager.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: [.fileSizeKey],
            options: [.skipsHiddenFiles]
        )) ?? []
        let uploadIDs = Set(urls.filter { $0.pathExtension == "upload" }.map {
            $0.deletingPathExtension().lastPathComponent
        })
        let pendingMultipartReserve = currentRecords.reduce(Int64(0)) { total, record in
            guard !uploadIDs.contains(record.id) else { return total }
            let audioURL = directory.appendingPathComponent(record.audioFilename)
            let audioBytes = Int64(
                (try? audioURL.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
            )
            return total + audioBytes + Self.maximumPerRecordAuxiliaryBytes
        }
        let allocated = allocatedBytesRecursively(at: directory)
        guard Self.projectedOutboxBytes(
            allocated: allocated,
            pendingMultipartReserve: pendingMultipartReserve,
            incomingBytes: incomingBytes
        )
                <= Self.maximumOutboxBytes else {
            throw AudioNoteOutboxError.outboxFull
        }
    }

    static func projectedOutboxBytes(
        allocated: Int64,
        pendingMultipartReserve: Int64,
        incomingBytes: Int64
    ) -> Int64 {
        allocated
            + pendingMultipartReserve
            + (incomingBytes * 2)
            + maximumPerRecordAuxiliaryBytes
    }

    private func allocatedBytesRecursively(at root: URL) -> Int64 {
        guard let enumerator = fileManager.enumerator(
            at: root,
            includingPropertiesForKeys: [.isRegularFileKey, .fileSizeKey],
            options: [.skipsHiddenFiles]
        ) else { return 0 }
        var total: Int64 = 0
        for case let url as URL in enumerator {
            guard let values = try? url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey]),
                  values.isRegularFile == true else { continue }
            total += Int64(values.fileSize ?? 0)
        }
        return total
    }

    private func secureDirectory() {
        do {
            try fileManager.createDirectory(
                at: directory,
                withIntermediateDirectories: true,
                attributes: [.protectionKey: Self.fileProtectionType]
            )
            try fileManager.setAttributes(
                [.protectionKey: Self.fileProtectionType],
                ofItemAtPath: directory.path
            )
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            var mutableDirectory = directory
            try mutableDirectory.setResourceValues(values)
        } catch {
            debugLog("[audio-note] could not secure outbox directory: \(error.localizedDescription)")
        }
    }

    private func protectFile(_ url: URL) throws {
        try fileManager.setAttributes(
            [.protectionKey: Self.fileProtectionType],
            ofItemAtPath: url.path
        )
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutableURL = url
        try mutableURL.setResourceValues(values)
    }

    private func records() -> [AudioNoteOutboxRecord] {
        let urls = (try? fileManager.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        )) ?? []
        var decoded: [AudioNoteOutboxRecord] = []
        for url in urls where url.pathExtension == "json" {
            guard let data = try? Data(contentsOf: url),
                  let record = try? JSONDecoder().decode(AudioNoteOutboxRecord.self, from: data)
            else {
                if let recovered = recoverRecordingWithBrokenMetadata(url) {
                    decoded.append(recovered)
                    continue
                }
                quarantineMalformedMetadata(url)
                continue
            }
            guard Self.recordIsSafe(record, metadataURL: url) else {
                // A decoded-but-unsafe destination/scope is not guessed or
                // rewritten into the current account. Preserve it in quarantine
                // rather than risking private-audio exfiltration or mis-scoping.
                quarantineMalformedMetadata(url)
                continue
            }
            decoded.append(record)
        }
        return decoded
    }

    private func recoverRecordingWithBrokenMetadata(_ metadataURL: URL) -> AudioNoteOutboxRecord? {
        let id = metadataURL.deletingPathExtension().lastPathComponent
        guard UUID(uuidString: id) != nil else { return nil }
        let audioFilename = "\(id).m4a"
        let audioURL = directory.appendingPathComponent(audioFilename)
        guard let values = try? audioURL.resourceValues(
            forKeys: [.fileSizeKey, .creationDateKey, .contentModificationDateKey]
        ), (values.fileSize ?? 0) > 0 else { return nil }
        let capturedAt = values.creationDate ?? values.contentModificationDate ?? Date()
        let recovered = AudioNoteOutboxRecord(
            id: id,
            capturedAt: Self.timestamp(capturedAt),
            durationMS: nil,
            sourceSurface: "ios_voice_note_recovered",
            audioFilename: audioFilename,
            transcript: nil,
            ready: true,
            destinationBaseURL: MagicianAccess.baseURL.absoluteString,
            principal: MagicianAccess.principal,
            workspace: MagicianAccess.workspace,
            provider: nil,
            attemptCount: 0,
            nextAttemptAt: nil,
            failedPermanently: false,
            lastError: "Recovered an audio-only note after its outbox metadata became unreadable."
        )
        do {
            try protectFile(audioURL)
            try persist(recovered)
            return recovered
        } catch {
            debugLog("[audio-note] could not recover broken metadata: \(error.localizedDescription)")
            return nil
        }
    }

    private func quarantineMalformedMetadata(_ url: URL) {
        let quarantine = directory.appendingPathComponent("Quarantine", isDirectory: true)
        try? fileManager.createDirectory(at: quarantine, withIntermediateDirectories: true)
        try? protectFile(quarantine)
        let sourceID = url.deletingPathExtension().lastPathComponent
        let quarantineID = "\(sourceID)-\(UUID().uuidString.lowercased())"
        let destination = quarantine
            .appendingPathComponent(quarantineID)
            .appendingPathExtension("json")
        try? fileManager.moveItem(at: url, to: destination)
        try? protectFile(destination)
        for pathExtension in ["m4a", "upload"] {
            let source = directory
                .appendingPathComponent(sourceID)
                .appendingPathExtension(pathExtension)
            guard fileManager.fileExists(atPath: source.path) else { continue }
            let protectedCopy = quarantine
                .appendingPathComponent(quarantineID)
                .appendingPathExtension(pathExtension)
            try? fileManager.moveItem(at: source, to: protectedCopy)
            try? protectFile(protectedCopy)
        }
        debugLog("[audio-note] quarantined unreadable outbox metadata \(url.lastPathComponent)")
    }

    private func loadRecord(id: String) -> AudioNoteOutboxRecord? {
        guard UUID(uuidString: id) != nil else { return nil }
        let url = directory.appendingPathComponent("\(id).json")
        guard let data = try? Data(contentsOf: url) else { return nil }
        guard let record = try? JSONDecoder().decode(AudioNoteOutboxRecord.self, from: data),
              Self.recordIsSafe(record, metadataURL: url) else { return nil }
        return record
    }

    private func persist(_ record: AudioNoteOutboxRecord) throws {
        let data = try JSONEncoder().encode(record)
        let url = directory.appendingPathComponent("\(record.id).json")
        try data.write(to: url, options: [.atomic, .completeFileProtection])
        try protectFile(url)
    }

    private func remove(_ record: AudioNoteOutboxRecord) {
        for url in [
            directory.appendingPathComponent(record.audioFilename),
            directory.appendingPathComponent("\(record.id).json"),
            directory.appendingPathComponent("\(record.id).upload")
        ] {
            try? fileManager.removeItem(at: url)
        }
    }

    private func publishStatuses() {
        let snapshot = records()
            .sorted {
                (Self.captureDate($0.capturedAt) ?? .distantPast)
                    > (Self.captureDate($1.capturedAt) ?? .distantPast)
            }
            .map { Self.outboxStatus(for: $0, active: activeRecordIDs.contains($0.id)) }
        DispatchQueue.main.async { [weak self] in self?.statuses = snapshot }
    }

    private static func captureDate(_ value: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = formatter.date(from: value) { return date }
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: value)
    }

    static func captureTimestampsMatch(_ first: String, _ second: String) -> Bool {
        guard let firstDate = captureDate(first), let secondDate = captureDate(second) else {
            return first == second
        }
        return abs(firstDate.timeIntervalSince(secondDate)) < 0.001
    }

    private static func timestamp(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.timeZone = .current
        formatter.dateFormat = "yyyy-MM-dd'T'HH:mm:ss.SSSXXXXX"
        return formatter.string(from: date)
    }

    static func recordIsSafe(
        _ record: AudioNoteOutboxRecord,
        metadataURL: URL,
        trustedBaseURL: URL = MagicianAccess.baseURL
    ) -> Bool {
        guard UUID(uuidString: record.id) != nil,
              metadataURL.deletingPathExtension().lastPathComponent == record.id,
              record.audioFilename == "\(record.id).m4a",
              captureDate(record.capturedAt) != nil else { return false }
        for scope in [record.principal, record.workspace].compactMap({ $0 }) {
            guard !scope.isEmpty,
                  scope != ".",
                  scope != "..",
                  scope.utf8.count <= 255,
                  !scope.contains("/"),
                  !scope.contains("\\"),
                  !scope.unicodeScalars.contains(where: {
                      CharacterSet.controlCharacters.contains($0)
                  })
            else { return false }
        }
        return validatedDestinationBaseURL(
            record.destinationBaseURL,
            trustedBaseURL: trustedBaseURL
        ) != nil
    }

    static func validatedDestinationBaseURL(
        _ raw: String?,
        trustedBaseURL: URL = MagicianAccess.baseURL
    ) -> URL? {
        guard let raw, !raw.isEmpty else { return trustedBaseURL }
        guard let url = URL(string: raw),
              url.user == nil,
              url.password == nil,
              url.query == nil,
              url.fragment == nil,
              url.path.isEmpty || url.path == "/",
              let scheme = url.scheme?.lowercased(),
              let host = url.host?.lowercased(),
              !host.isEmpty else { return nil }
        if scheme == "http", ["localhost", "127.0.0.1", "::1"].contains(host) {
            return url
        }
        if scheme == trustedBaseURL.scheme?.lowercased(),
           host == trustedBaseURL.host?.lowercased(),
           url.port == trustedBaseURL.port {
            return url
        }
        return nil
    }

    private static func appendField(
        _ name: String,
        value: String,
        boundary: String,
        to body: inout Data
    ) {
        append("--\(boundary)\r\n", to: &body)
        append("Content-Disposition: form-data; name=\"\(name)\"\r\n\r\n", to: &body)
        append("\(value)\r\n", to: &body)
    }

    private static func append(_ value: String, to body: inout Data) {
        body.append(Data(value.utf8))
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        worker.async { [weak self] in
            guard let self else { return }
            var body = self.responseBodies[dataTask.taskIdentifier] ?? Data()
            if body.count < 64 * 1_024 {
                body.append(data.prefix((64 * 1_024) - body.count))
            }
            self.responseBodies[dataTask.taskIdentifier] = body
        }
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didCompleteWithError error: Error?
    ) {
        worker.async { [weak self] in
            guard let self else { return }
            let wasTracked = self.activeTaskIDs.remove(task.taskIdentifier) != nil
            if wasTracked, let id = task.taskDescription {
                self.activeRecordIDs.remove(id)
            }
            let data = self.responseBodies.removeValue(forKey: task.taskIdentifier) ?? Data()
            // Canceled duplicates/orphans discovered during restoration still
            // deliver completion callbacks. They do not own a record and must
            // never mutate retry state for the accepted task.
            guard wasTracked else {
                self.publishStatuses()
                if self.restoredBackgroundTasks { self.drain() }
                return
            }
            let recordID = task.taskDescription
            guard let id = recordID, let record = self.loadRecord(id: id) else {
                if let id = recordID, UUID(uuidString: id) != nil {
                    try? self.fileManager.removeItem(
                        at: self.directory.appendingPathComponent("\(id).upload")
                    )
                }
                self.publishStatuses()
                self.drain()
                return
            }
            let audioURL = self.directory.appendingPathComponent(record.audioFilename)
            let expectedBytes = (try? audioURL.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
            let status = (task.response as? HTTPURLResponse)?.statusCode
            switch Self.uploadOutcome(
                recordID: record.id,
                expectedCapturedAt: record.capturedAt,
                expectedBytes: expectedBytes,
                data: data,
                status: status,
                error: error
            ) {
            case .success:
                self.remove(record)
                self.publishStatuses()
                self.drain()
            case .retry(let message):
                try? self.fileManager.removeItem(
                    at: self.directory.appendingPathComponent("\(record.id).upload")
                )
                if self.markRetry(record, message: message) {
                    self.drain()
                }
            case .permanentFailure(let message):
                try? self.fileManager.removeItem(
                    at: self.directory.appendingPathComponent("\(record.id).upload")
                )
                if self.markPermanentFailure(record, message: message) {
                    self.drain()
                }
            }
        }
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        // Never turn a Cloudflare/login redirect into a false 200 success.
        completionHandler(nil)
    }

    func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
        worker.async { [weak self] in
            guard let self else { return }
            self.backgroundCompletionLock.lock()
            let completion = self.backgroundCompletionHandler
            self.backgroundCompletionHandler = nil
            self.backgroundCompletionLock.unlock()
            DispatchQueue.main.async { completion?() }
        }
    }
}

/// Result of one automatically bounded Ambient Dictation capture. `noSpeech`
/// is an ordinary false wake/follow-up expiry; a failed transcription is a
/// broken turn and must not be presented as the user choosing silence.
enum AmbientDictationCaptureResult: Equatable {
    case transcript(String)
    case noSpeech
    case failedToTranscribe
}

/// Pure utterance-boundary reducer for Ambient Dictation's file recorder.
///
/// It is deliberately iterative: the 10 Hz meter owns the clock and feeds one
/// sample at a time. No callback recursively starts another sample or another
/// conversation, so a long armed window cannot grow the stack with each turn.
struct AmbientDictationSilenceGate: Equatable {
    enum Event: Equatable {
        case none
        case speechBegan
        case finishUtterance
        case noSpeech
    }

    static let speechThresholdDB: Float = -42
    static let trailingSilenceSeconds: TimeInterval = 1.1
    static let noSpeechSeconds: TimeInterval = 8
    static let maximumUtteranceSeconds: TimeInterval = 45

    let startedAt: TimeInterval
    private(set) var heardSpeech = false
    private(set) var lastSpeechAt: TimeInterval?

    mutating func observe(powerDB: Float, at now: TimeInterval) -> Event {
        guard now >= startedAt else { return .none }
        let elapsed = now - startedAt
        if powerDB >= Self.speechThresholdDB {
            lastSpeechAt = now
            if !heardSpeech {
                heardSpeech = true
                return .speechBegan
            }
        }
        if heardSpeech {
            if elapsed >= Self.maximumUtteranceSeconds {
                return .finishUtterance
            }
            if let lastSpeechAt,
               now - lastSpeechAt >= Self.trailingSilenceSeconds {
                return .finishUtterance
            }
            return .none
        }
        return elapsed >= Self.noSpeechSeconds ? .noSpeech : .none
    }
}

/// The concrete capture implementation that successfully started. This stays
/// independent of user preference because Auto may choose a different route on
/// each attempt as permissions and recognizer availability change.
enum DictationCaptureRoute: Equatable {
    case idle
    case file
    case live

    var finishAction: DictationFinishAction {
        switch self {
        case .idle: return .none
        case .file: return .transcribeFile
        case .live: return .finalizeLive
        }
    }
}

enum DictationFinishAction: Equatable {
    case none
    case transcribeFile
    case finalizeLive
}

enum AmbientDictationStartAction: Equatable {
    case createInputGraph
    case resumeRetainedInputGraph
    case reject
}

enum DictationTranscriptionRoute: Equatable {
    case onDevice(allowsCloudFallback: Bool)
    case cloud
    case unavailable
}

enum AmbientDictationPCMError: Error {
    case empty
    case incompleteSample
    case tooLarge
}

/// One immutable user turn, already resampled by `AmbientMicEngine` to the STT
/// contract. Encoding owns the WAV header, so there is never a second reader
/// racing a recorder that has not finalised its `data` chunk yet.
struct AmbientDictationPCMSnapshot: Equatable {
    static let sampleRate: UInt32 = 16_000
    static let channelCount: UInt16 = 1
    static let bitsPerSample: UInt16 = 16

    let pcm16LE: Data

    func wavData() throws -> Data {
        guard !pcm16LE.isEmpty else { throw AmbientDictationPCMError.empty }
        guard pcm16LE.count.isMultiple(of: MemoryLayout<Int16>.size) else {
            throw AmbientDictationPCMError.incompleteSample
        }
        let payloadCount = UInt64(pcm16LE.count)
        guard payloadCount <= UInt64(UInt32.max) - 36 else {
            throw AmbientDictationPCMError.tooLarge
        }

        let blockAlign = Self.channelCount * (Self.bitsPerSample / 8)
        let byteRate = Self.sampleRate * UInt32(blockAlign)
        var wav = Data()
        wav.reserveCapacity(44 + pcm16LE.count)
        wav.append(Data("RIFF".utf8))
        Self.append(UInt32(36 + pcm16LE.count), to: &wav)
        wav.append(Data("WAVEfmt ".utf8))
        Self.append(UInt32(16), to: &wav)
        Self.append(UInt16(1), to: &wav)
        Self.append(Self.channelCount, to: &wav)
        Self.append(Self.sampleRate, to: &wav)
        Self.append(byteRate, to: &wav)
        Self.append(blockAlign, to: &wav)
        Self.append(Self.bitsPerSample, to: &wav)
        wav.append(Data("data".utf8))
        Self.append(UInt32(pcm16LE.count), to: &wav)
        wav.append(pcm16LE)
        return wav
    }

    private static func append(_ value: UInt16, to data: inout Data) {
        var littleEndian = value.littleEndian
        withUnsafeBytes(of: &littleEndian) { data.append(contentsOf: $0) }
    }

    private static func append(_ value: UInt32, to data: inout Data) {
        var littleEndian = value.littleEndian
        withUnsafeBytes(of: &littleEndian) { data.append(contentsOf: $0) }
    }
}

/// Thread-safe gate over the continuously running ambient input graph. The
/// audio render thread appends PCM only while a turn is listening; STT and TTS
/// leave the graph alive but the gate closed, preventing the assistant from
/// transcribing itself. Each `beginTurn` replaces the prior buffer, so turn N
/// can never leak into turn N+1.
final class AmbientDictationPCMAccumulator: @unchecked Sendable {
    private struct State {
        var accepting = false
        var hasFrames = false
        var pcm16LE = Data()
        var averagePowerDB: Float = -160
    }

    private let state = OSAllocatedUnfairLock(initialState: State())

    var averagePowerDB: Float {
        state.withLock { $0.averagePowerDB }
    }

    var bufferedByteCount: Int {
        state.withLock { $0.pcm16LE.count }
    }

    var hasFrames: Bool {
        state.withLock { $0.hasFrames }
    }

    func beginTurn() {
        state.withLock {
            $0.accepting = true
            $0.hasFrames = false
            $0.pcm16LE.removeAll(keepingCapacity: true)
            $0.averagePowerDB = -160
        }
    }

    /// Returns true only for the first accepted frame in the current turn. The
    /// audio callback uses this edge to prove that input IO is genuinely running;
    /// engine state flags alone are not a readiness signal on physical devices.
    @discardableResult
    func append(_ frame: Data) -> Bool {
        guard !frame.isEmpty, frame.count.isMultiple(of: MemoryLayout<Int16>.size) else {
            return false
        }
        let power = Self.averagePowerDB(of: frame)
        return state.withLock {
            guard $0.accepting else { return false }
            let isFirstFrame = !$0.hasFrames
            $0.hasFrames = true
            $0.pcm16LE.append(frame)
            $0.averagePowerDB = power
            return isFirstFrame
        }
    }

    func finishTurn() -> AmbientDictationPCMSnapshot? {
        state.withLock {
            guard $0.accepting, !$0.pcm16LE.isEmpty else {
                $0.accepting = false
                $0.hasFrames = false
                $0.averagePowerDB = -160
                return nil
            }
            $0.accepting = false
            $0.hasFrames = false
            $0.averagePowerDB = -160
            let snapshot = AmbientDictationPCMSnapshot(pcm16LE: $0.pcm16LE)
            $0.pcm16LE = Data()
            return snapshot
        }
    }

    func reset() {
        state.withLock { $0 = State() }
    }

    private static func averagePowerDB(of pcm16LE: Data) -> Float {
        var sumSquares: Double = 0
        let count = pcm16LE.count / MemoryLayout<Int16>.size
        pcm16LE.withUnsafeBytes { bytes in
            for index in 0..<count {
                let sample = bytes.loadUnaligned(
                    fromByteOffset: index * MemoryLayout<Int16>.size,
                    as: Int16.self
                ).littleEndian
                let normalised = Double(sample) / Double(Int16.max)
                sumSquares += normalised * normalised
            }
        }
        guard count > 0 else { return -160 }
        let rms = sqrt(sumSquares / Double(count))
        guard rms > 0 else { return -160 }
        return max(-160, Float(20 * log10(rms)))
    }
}

/// Multipart metadata follows the actual temporary-file container. Ambient
/// segments are WAV; ordinary composer dictation and voice notes remain M4A.
struct DictationAudioUpload: Equatable {
    let filename: String
    let contentType: String

    static func file(at url: URL) -> Self {
        switch url.pathExtension.lowercased() {
        case "wav", "wave":
            return Self(filename: "dictation.wav", contentType: "audio/wav")
        default:
            return Self(filename: "dictation.m4a", contentType: "audio/m4a")
        }
    }
}

/// One-shot ownership for a recorded file while Speech and cloud STT callbacks
/// race cancellation. The reducer is intentionally framework-free so its stale
/// callback and cleanup guarantees can be covered without microphone/network IO.
struct DictationTranscriptionLease: Equatable {
    enum Stage: Equatable {
        case captured
        case onDevice
        case resolvingOnDevice
        case cloud
    }

    private(set) var generation = 0
    private(set) var activeGeneration: Int?
    private(set) var activeFileURL: URL?
    private(set) var stage: Stage?

    mutating func begin(fileURL: URL) -> Int {
        generation += 1
        activeGeneration = generation
        activeFileURL = fileURL
        stage = .captured
        return generation
    }

    func accepts(_ candidate: Int) -> Bool {
        activeGeneration == candidate
    }

    mutating func beginOnDevice(_ candidate: Int) -> Bool {
        guard accepts(candidate), stage == .captured else { return false }
        stage = .onDevice
        return true
    }

    mutating func claimOnDeviceResult(_ candidate: Int) -> Bool {
        guard accepts(candidate), stage == .onDevice else { return false }
        stage = .resolvingOnDevice
        return true
    }

    mutating func beginCloud(_ candidate: Int) -> URL? {
        guard accepts(candidate),
              stage == .captured || stage == .resolvingOnDevice,
              let activeFileURL else { return nil }
        stage = .cloud
        let url = activeFileURL
        self.activeFileURL = nil
        return url
    }

    func isCloudActive(_ candidate: Int) -> Bool {
        accepts(candidate) && stage == .cloud
    }

    mutating func finish(_ candidate: Int) -> URL? {
        guard accepts(candidate) else { return nil }
        let url = activeFileURL
        activeGeneration = nil
        activeFileURL = nil
        stage = nil
        return url
    }

    mutating func cancel() -> URL? {
        generation += 1
        let url = activeFileURL
        activeGeneration = nil
        activeFileURL = nil
        stage = nil
        return url
    }
}
