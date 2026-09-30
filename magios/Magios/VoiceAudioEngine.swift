// `@preconcurrency`: AVFAudio's `AVAudioPCMBuffer` isn't `Sendable`, but the
// capture buffer we hand back to the converter's `@Sendable` input block never
// crosses an isolation boundary (the converter calls it synchronously). Silences
// the spurious Sendable warning without changing behavior.
@preconcurrency import AVFoundation
import Foundation
import os
import UIKit
// `CACurrentMediaTime` — the render-thread-safe clock used by the half-duplex
// fallback (no allocation, no main-actor hop, unlike `Date`).
import QuartzCore

/// **Who owns the shared `AVAudioSession` for the duration of a call** — and
/// therefore whether a microphone can ever be reopened afterwards from the
/// background.
///
/// It governs BOTH ends of the call, and the outbound end was added second, after
/// a device reproduction. Originally this decided only what a *teardown* did; the
/// name still reads that way, and the widening is deliberate rather than an
/// overload: a call that must not hand the session back on the way out is exactly
/// a call that does not own it on the way in, and expressing that as two
/// independent flags is how one of them gets set and the other forgotten.
///
/// | | `start` configures + activates | `stop` deactivates |
/// |---|---|---|
/// | `.release` | yes | yes |
/// | `.keepActive` | **no** | **no** |
///
/// Named, and required at every call site rather than defaulted, because the two
/// answers are not variations on a theme: one of them ends a call, and the other
/// one decides whether an armed ambient window survives it. Apple DTS (thread
/// 826462, May 2026) states the rule as a recipe — activate the session in the
/// foreground *once* and never deactivate it, and the engine can then be started
/// and stopped from the background indefinitely. **Deactivate it and it cannot be
/// reactivated from the background at all**, at which point an armed window dies
/// silently at the first wake word: the one moment nobody is watching a screen.
///
/// A defaulted `Bool` is how that gets passed wrong later — the ambient call site
/// would read as an omission rather than as the decision it is, and the failure it
/// causes is invisible in the simulator, invisible in the tests that do not pin it,
/// and reads on a device as a Vosk problem. See
/// `docs/components/magios/ambient-call-sink.md` — which carries the invariant to
/// check after any edit here — and the design's §13.3 / §15.
enum VoiceSessionDisposition: Equatable {
    /// Deactivate the shared session on the way out, notifying others. What the
    /// in-app Live call has always done and must keep doing: it borrowed the
    /// session for a call, and dictation, chat narration and observation all need
    /// it back.
    case release
    /// Leave the shared session **exactly as the armed window left it** — neither
    /// configured on the way in nor deactivated on the way out. **Required for the
    /// ambient wake handoff.**
    ///
    /// The session belongs to the armed window, not to the call: `AmbientMicEngine`
    /// configures it and activates it in the FOREGROUND at arm time and never undoes
    /// it, and the window's return leg (`AmbientController.resumeSpotting`) restarts
    /// the spotting tap from the background, where a re-activation would be refused.
    ///
    /// **The inbound half of this case exists because of a device reproduction, not
    /// a theory.** Arming worked; the wake phrase was heard while the app was
    /// backgrounded, killed or the screen locked — which proves the session was
    /// active and the spotting tap ran correctly off screen — and then the
    /// microphone was lost. The failure was the handoff itself: `start` changed the
    /// session's *mode*, re-stated a preferred sample rate and called
    /// `setActive(true)` from the background, then rebuilt the audio unit with voice
    /// processing on top of it. The input node came back with a degenerate format,
    /// `AmbientMicEngine.installTap`'s guard refused it, and `resumeSpotting`
    /// disarmed with "Lost the microphone."
    case keepActive

    /// Whether a `start` may configure and activate the shared session, or must
    /// take it exactly as it is.
    ///
    /// Named rather than left as an inline `== .release`, because the skip is a
    /// stated decision about ownership and the branch is unobservable on the
    /// hardware where it matters: reading `if disposition == .release` at the call
    /// site would make the ambient path look like a special case of a teardown flag.
    /// See `VoiceAudioEngine.configureSharedSessionIfOwned`.
    var configuresSharedSession: Bool {
        switch self {
        case .release: return true
        case .keepActive: return false
        }
    }
}

/// Render-thread state for deciding when speaker playback must not be forwarded
/// back to the voice provider as microphone input.
struct VoiceCaptureGateState: Equatable {
    private(set) var initialAssistantPlaybackPending = true

    /// Whether queued reply audio is still coming out of the speaker, judged
    /// against the playback clock's drain deadline plus a tail.
    ///
    /// `drainsAt` is `AssistantPlaybackClock.idleAtUptime` — the accumulated
    /// duration of every frame queued — never a stamp of when frames arrived.
    /// A realtime provider streams a reply 5–10× faster than it plays, so any
    /// arrival-based measure reads "done" while seconds of speech are still in
    /// the speaker; this function is the gate's basis so it can never again be
    /// built on that fallacy. Zero means nothing was ever queued. Pure and
    /// static so the arithmetic is assertable without an audio graph.
    static func playbackAudible(
        drainsAt: TimeInterval, tail: TimeInterval, now: TimeInterval
    ) -> Bool {
        drainsAt > 0 && now < drainsAt + tail
    }

    mutating func shouldSuppressCapture(
        playbackStarted: Bool,
        initialPlaybackActive: Bool,
        assistantSpeaking: Bool,
        echoCancellationActive: Bool
    ) -> Bool {
        if initialAssistantPlaybackPending, playbackStarted {
            if initialPlaybackActive {
                return true
            }
            initialAssistantPlaybackPending = false
        }
        return !echoCancellationActive && assistantSpeaking
    }
}

/// Full-duplex audio core for the Live voice call (Task 4).
///
/// Captures the microphone and resamples it to 24 kHz mono int16-LE frames for
/// the upstream WebSocket, and plays incoming 24 kHz int16-LE PCM downstream —
/// with **hardware echo cancellation**. This is the device-verified half of the
/// call: `SFSpeech`/`AVAudioEngine` realtime audio cannot run in the simulator,
/// so verification here is "compiles + existing tests stay green".
///
/// Wiring (done by the ViewModel, not here): the transport's `onIncomingAudio`
/// hook forwards downstream frames to `play(_:)`, and `start(onFrameOut:)`'s
/// callback forwards captured frames to `RealtimeVoiceClient.sendAudio(_:)`.
///
/// ## Echo cancellation
/// The shared `AVAudioSession` is `.playAndRecord` with mode **`.voiceChat`**,
/// which arms the hardware AEC (voice-processing I/O). Without it, the mic would
/// re-capture the speaker and feed the model its own voice (feedback). This is
/// the single most important property for a usable full-duplex call.
///
/// **Who puts it there depends on the caller.** An in-app call configures the
/// session itself. An **ambient** call does not touch it at all: the armed window
/// configured it — with the same triple, `.voiceChat` included — in the foreground
/// at arm time, and re-configuring or re-activating it from the background is what
/// used to lose the microphone at the first wake word. See
/// `VoiceSessionDisposition` and `configureSharedSessionIfOwned`. What still runs on
/// both paths is `setVoiceProcessingEnabled` on this engine's own I/O nodes, which
/// is per-engine state and is what actually inserts the AEC unit.
///
/// ## Resampling
/// The hardware input node runs at whatever rate the device/route dictates
/// (commonly 44.1 or 48 kHz). An `AVAudioConverter` (built lazily from the live
/// input format → the 24 kHz mono target) resamples each captured buffer down to
/// the transport rate before we extract float samples and encode int16-LE.
@MainActor
final class VoiceAudioEngine: ObservableObject {

    /// Input RMS in 0…1, published for the orb. Written on the main actor by a
    /// ~20 Hz timer that reads `levelValue` (which the render thread updates) —
    /// so the render thread never touches this `@Published` property directly.
    @Published private(set) var level: Float = 0

    /// The render thread's latest RMS, behind a lock (read by the level timer).
    private let levelValue = OSAllocatedUnfairLock<Float>(initialState: 0)
    private var levelTimer: Timer?

    /// Push-to-talk gate: written from the main actor (mute/PTT), read on the
    /// audio render thread. Lock-guarded so the cross-thread access is race-free.
    /// `nonisolated` so the render-thread capture path can read it legally.
    private let mutedFlag = OSAllocatedUnfairLock<Bool>(initialState: false)
    nonisolated var muted: Bool {
        get { mutedFlag.withLock { $0 } }
        set { mutedFlag.withLock { $0 = newValue } }
    }

    /// Same subsystem/category as `VoiceCallViewModel`: this engine is that
    /// call's audio half on both the in-app and the ambient path.
    private let log = Logger(subsystem: "ai.magicbeans.magios", category: "voice.call")

    /// True once the voice-processing (AEC) unit is armed on the engine's I/O
    /// nodes. When false the call is running WITHOUT echo cancellation and the
    /// half-duplex fallback below is doing the work instead.
    @Published private(set) var echoCancellationActive = false

    /// Mirrors `!echoCancellationActive` for the render thread. Only when AEC
    /// could not be armed do we gate capture during assistant playback —
    /// half-duplex costs barge-in, so it is a fallback, never the default.
    private let halfDuplexFallback = OSAllocatedUnfairLock<Bool>(initialState: false)

    /// Hardware AEC needs the first speaker playback to learn the device's
    /// speaker-to-microphone path. Until that first reply and its acoustic tail
    /// finish, suppress capture so part of the reply cannot become the next user
    /// turn. Later replies retain normal full-duplex barge-in when AEC is active.
    private let captureGate = OSAllocatedUnfairLock<VoiceCaptureGateState>(
        initialState: VoiceCaptureGateState()
    )

    /// When queued downstream audio will have finished coming out of the
    /// speaker, so the render thread can tell whether the assistant is
    /// currently AUDIBLE — not merely whether frames recently arrived.
    ///
    /// This used to be an arrival stamp with a 0.35 s debounce, which is the
    /// wrong question answered precisely (see `AssistantPlaybackClock`'s doc):
    /// the provider streams a reply 5–10× faster than realtime, so a stamp
    /// taken at arrival ran dry while seconds of TTS were still playing — and
    /// with AEC unarmed, every second of audible playback past the gate went
    /// upstream as raw echo and came back as the model answering itself.
    /// The clock instead accumulates each queued frame's own PCM duration, so
    /// the gate suppresses until the speaker actually runs dry.
    ///
    /// Fed `CACurrentMediaTime()` on both the queue side (`play`) and the read
    /// side (`processCapture`); the clock takes `now` as a parameter, and the
    /// one rule is that both sides share a timebase.
    private let playbackClock = OSAllocatedUnfairLock<AssistantPlaybackClock>(
        initialState: AssistantPlaybackClock()
    )

    /// Grace past the drain deadline before capture reopens in the half-duplex
    /// fallback: the speaker's acoustic decay allowance, plus cover for a brief
    /// provider stall where the queue runs dry mid-utterance. The gap between
    /// frames of one normally-streamed reply needs no covering any more — the
    /// queued durations accumulate, so mid-reply the deadline is ahead of now.
    nonisolated private static let halfDuplexHangover: CFTimeInterval = 0.35
    /// The first playback gets a longer tail past its drain while the AEC
    /// converges on the device's speaker-to-microphone path.
    nonisolated private static let initialPlaybackHangover: CFTimeInterval = 0.65

    // MARK: - Audio graph

    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()

    /// The transport format: 24 kHz mono float32, non-interleaved. `AVAudioFormat`
    /// is `Sendable`, so this immutable `let` is already usable from the nonisolated
    /// capture path without further annotation.
    private let targetFormat = AVAudioFormat(
        commonFormat: .pcmFormatFloat32,
        sampleRate: 24_000,
        channels: 1,
        interleaved: false
    )!

    /// Converter + reused output buffer, touched ONLY on the audio render thread
    /// (in `processCapture`) — single-threaded access, so `nonisolated(unsafe)`
    /// opts them out of actor isolation without a lock. Built once + rebuilt only
    /// on a route/format change (not per buffer); the output buffer is reused.
    nonisolated(unsafe) private var converter: AVAudioConverter?
    nonisolated(unsafe) private var converterInputFormat: AVAudioFormat?
    nonisolated(unsafe) private var conversionBuffer: AVAudioPCMBuffer?

    private var isRunning = false
    /// Set while an interruption has paused playback so `.ended`/`.shouldResume`
    /// knows to restart it.
    private var interrupted = false

    // MARK: - Session-disposition observability
    //
    // Two counters, and they exist for one reason: the branch that decides whether
    // the shared session survives a teardown is otherwise unreachable from any
    // test. `stop()` returns early unless `isRunning`, and `isRunning` can only be
    // set by a successful `start()` — which needs a microphone, and
    // `AVAudioEngine` capture has no input in the simulator. So a guard placed
    // after that gate would pass forever without ever executing the line it
    // claims to protect, which is exactly how this feature previously earned a
    // false clean bill of health.

    /// What the most recent `start`/`stop` **asked** for, recorded before any
    /// guard so the request is observable even when the operation itself is a
    /// no-op. This answers "did the ambient path state its intent correctly?".
    private(set) var lastSessionDisposition: VoiceSessionDisposition?

    /// How many times this engine has **actually** deactivated the shared session.
    /// This answers the stronger question: "can the ambient path deactivate the
    /// session by any route?" — for which the only acceptable value is zero.
    /// "Actually" is enforced, not aspirational: a `setActive(false)` a busy
    /// session refuses does not move it, so the count cannot claim a
    /// deactivation that never happened.
    private(set) var sessionReleaseCount = 0

    /// How many times this engine has **actually** re-configured and re-activated
    /// the shared session, for the same reason and with the same acceptable value on
    /// the ambient path. Same idiom as `SpeechSynthesizer.sessionConfigureCount`,
    /// which exists because a *re-categorisation* takes a microphone away just as
    /// effectively as a deactivation (design §15's third correction).
    ///
    /// It is the counter that would have caught this feature's headline bug: the
    /// wrong value here is not a crash, it is a wake phrase that works on screen and
    /// loses the microphone off it.
    private(set) var sessionConfigureCount = 0

    /// How many times playback has been flushed by a server-side interrupt
    /// (`flushPlayback`). Same idiom as the two counters above: the flush's
    /// audible half (`player.stop()`) needs a started engine, which the
    /// simulator cannot give, so the request is counted where the attachment
    /// guard cannot hide it — it is what lets a test prove the interrupt hook
    /// actually reaches this engine.
    private(set) var playbackFlushCount = 0

    /// Physical queue drain, independent of the provider's generation-end event.
    var hasPendingPlayback: Bool {
        playbackClock.withLock { $0.idleAtUptime > CACurrentMediaTime() }
    }

    // MARK: - Lifecycle

    init() {
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleInterruption(_:)),
            name: AVAudioSession.interruptionNotification,
            object: nil
        )
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }

    /// Configure the session (with AEC), build the graph, install the capture
    /// tap, and start streaming. `onFrameOut` receives each 24 kHz int16-LE
    /// frame ready for the upstream WS.
    ///
    /// `session` governs **both** the shared session's configuration on the way in
    /// and the failure path on the way out. It is required rather than defaulted for
    /// the same reason `stop(session:)`'s is: a call that fails to start while an
    /// ambient window is armed must not take the window's session down with it, and
    /// that has to be a decision at the call site rather than a value someone forgot.
    ///
    /// A `.keepActive` start configures **nothing** — see
    /// `configureSharedSessionIfOwned`. Everything below that line still runs,
    /// including `setVoiceProcessingEnabled` on both nodes, which is per-engine
    /// rather than per-session and is load-bearing on every path.
    func start(session: VoiceSessionDisposition, onFrameOut: @escaping (Data) -> Void) throws {
        lastSessionDisposition = session
        // Re-entrancy guard: a second `start()` without `stop()` would install a
        // second tap on the same bus → ObjC exception crash.
        guard !isRunning else { return }
        do {
            try configureSharedSessionIfOwned(session)

            let input = engine.inputNode
            // Arm the echo canceller ON THE ENGINE'S I/O NODES.
            //
            // **This runs on EVERY path, including a `.keepActive` start that
            // configured nothing, and it must.** The session being `.voiceChat`
            // is NOT sufficient by itself: it configures session routing, but
            // `AVAudioEngine`'s input node still runs a plain RemoteIO unit, so
            // the capture tap below receives RAW mic audio. With the speaker
            // carrying the assistant's voice (`.defaultToSpeaker`) and the mic
            // held open (PTT Off), that raw audio contains the assistant itself
            // — server VAD then scores it as a new user turn and the model
            // answers its own reply in a loop. Two engines share one session, so
            // there is nothing the armed window could have armed on this one's
            // behalf: this is per-`AVAudioEngine` state, set on this engine's own
            // input and output nodes.
            //
            // `setVoiceProcessingEnabled` is what actually inserts the
            // voice-processing (AEC) unit into the graph. It MUST be armed
            // before `outputFormat(forBus:)` is read below, because enabling it
            // changes the input node's format.
            //
            // It is now the ONLY step in this method that touches the audio
            // hardware on an ambient call, which makes it the whole of the
            // remaining device risk — hence device checklist item 2 in
            // `docs/components/magios/ambient-call-sink.md`. If it fails it is not
            // fatal (the `catch` below falls back to half-duplex) but it is not
            // silent either: `echoCancellationActive` publishes the answer.
            do {
                try input.setVoiceProcessingEnabled(true)
                try engine.outputNode.setVoiceProcessingEnabled(true)
                echoCancellationActive = true
                // Which echo defense this call runs is the single most
                // diagnostic on-device bit for the hears-itself class of bug,
                // so both outcomes log — a `Logger` line rather than a DEBUG
                // print, because the devices that hit the fallback are
                // release builds.
                log.info("voice processing armed (hardware AEC active)")
            } catch {
                // Unsupported route or simulator. Keep the call usable by
                // falling back to half-duplex capture gating on the playback
                // clock (see `playbackClock`), which costs barge-in but
                // prevents the feedback loop. Never fatal: a call without AEC
                // still beats no call at all.
                echoCancellationActive = false
                log.warning(
                    "voice processing unavailable — half-duplex playback-clock gate active: \(error.localizedDescription, privacy: .public)"
                )
            }
            // Read the main-actor property into a local first: `withLock`'s
            // closure is `Sendable` and cannot capture actor-isolated state.
            let aecArmed = echoCancellationActive
            halfDuplexFallback.withLock { $0 = !aecArmed }
            captureGate.withLock { $0 = VoiceCaptureGateState() }
            // Same defensive reset as the gate above: a teardown that never
            // reached `stop()` can leave a drain deadline behind, and a stale
            // deadline gates a live microphone against audio that will never
            // play — the inverse of the echo bug the clock exists to stop.
            playbackClock.withLock { $0.reset() }

            // Playback path: player → main mixer at the 24 kHz target format.
            // Idempotent attach (so a retry after a failed start doesn't
            // double-attach the node, which throws).
            if player.engine == nil {
                engine.attach(player)
                engine.connect(player, to: engine.mainMixerNode, format: targetFormat)
            }

            // Capture path: tap the input node at its own (hardware) format and
            // resample to 24 kHz mono inside the tap. Read the format AFTER
            // voice processing is armed — it differs once AEC is inserted.
            let inputFormat = input.outputFormat(forBus: 0)
            input.installTap(onBus: 0, bufferSize: 1024, format: inputFormat) { [weak self] buffer, _ in
                self?.processCapture(buffer, onFrameOut: onFrameOut)
            }

            engine.prepare()
            try engine.start()
            player.play()
            isRunning = true
            interrupted = false
            startLevelTimer()
        } catch {
            // Exception-safe: undo any partial setup so a stuck tap / active
            // session doesn't leak (and `stop()`'s `isRunning` guard can't reach it).
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
            // The caller's disposition applies here too. A `.keepActive` caller has
            // an armed ambient window behind it: deactivating on the way out of a
            // failed connect would turn a RECOVERABLE failure (fall back to armed,
            // keep listening for the wake word) into a dead window, since nothing
            // could reactivate the session from the background afterwards.
            releaseSessionIfRequested(session)
            throw error
        }
    }

    /// Queue a downstream 24 kHz int16-LE PCM frame for seamless playback.
    func play(_ data: Data) {
        let floats = VoicePCM.int16ToFloat(data)
        guard !floats.isEmpty,
              let buffer = AVAudioPCMBuffer(
                pcmFormat: targetFormat,
                frameCapacity: AVAudioFrameCount(floats.count)
              ),
              let channel = buffer.floatChannelData
        else { return }

        floats.withUnsafeBufferPointer { src in
            channel[0].update(from: src.baseAddress!, count: floats.count)
        }
        buffer.frameLength = AVAudioFrameCount(floats.count)

        // Advance the drain deadline BEFORE scheduling, so the render thread
        // never sees assistant audio reach the speaker while the gate still
        // reads "not speaking". `data` is transport-rate PCM16, which is
        // exactly the byte→seconds arithmetic the clock is written for.
        let now = CACurrentMediaTime()
        let frameBytes = data.count
        let sampleRate = Int(targetFormat.sampleRate)
        playbackClock.withLock {
            _ = $0.queue(frameBytes: frameBytes, sampleRate: sampleRate, now: now)
        }

        // Contiguous scheduling → the player node stitches frames into one
        // gapless stream.
        player.scheduleBuffer(buffer, completionHandler: nil)
        if isRunning, !player.isPlaying {
            player.play()
        }
    }

    /// Drop everything queued for the speaker and reset the playback clock —
    /// the client half of a server-side interrupt.
    ///
    /// The server collapses a reply's self-echo window the moment it
    /// interrupts the reply (`magician-media/src/media_rails/self_echo.rs`,
    /// `note_assistant_audio_done` / `truncate_playback`): from that instant it
    /// stops accounting for bytes it already streamed, while this player may
    /// still hold seconds of them. A client that lets that orphaned tail keep
    /// sounding re-opens the exact gap the collapse closed — on an
    /// armed-but-leaky-AEC device its echo finalizes after the collapsed
    /// window plus the server's tail and is admitted as a user turn, which is
    /// what re-seeds the self-hearing loop. Flushing is the client keeping the
    /// server's promise.
    ///
    /// `player.stop()` both halts the node and discards its scheduled buffers.
    /// It is deliberately NOT re-`play()`ed here: `play(_:)` already restarts
    /// a non-playing node when the next reply's first frame is queued — the
    /// same guard every fresh call relies on — and restarting against an
    /// engine the system paused (a session interruption can be in flight when
    /// the server's event lands) raises an AVFAudio exception. The clock reset
    /// is the other half of the contract: after the flush the drain deadline
    /// is a claim about silence (the same reasoning as `stop(session:)`), and
    /// in the half-duplex fallback a stale deadline would gate a live
    /// microphone against audio that will never play.
    ///
    /// Idempotent and safe with nothing playing — an interrupt routinely lands
    /// after the queue has already drained. Stopping an idle node is a no-op,
    /// resetting a zero clock is one too, and an unattached node (no
    /// successful `start` yet, so nothing was ever queued) is left untouched:
    /// driving it would be the uncatchable AVFAudio exception the wiring
    /// tests document.
    func flushPlayback() {
        playbackFlushCount += 1
        if player.engine != nil {
            player.stop()
        }
        playbackClock.withLock { $0.reset() }
    }

    /// Tear down the capture tap, stop playback and the engine — and hand the
    /// shared audio session back to the system, or **not**, as `session` says.
    ///
    /// The parameter has no default on purpose. `.release` is what this method
    /// always did and what the in-app call still needs; `.keepActive` is what the
    /// ambient wake handoff's return leg needs, because this is the moment that
    /// decides whether `AmbientController.resumeSpotting` can reopen a microphone
    /// from the background. See `VoiceSessionDisposition`.
    func stop(session: VoiceSessionDisposition) {
        // Recorded before the guard — see the counters' declaration for why.
        lastSessionDisposition = session
        // The stamp bracketing the instant the shared session must survive.
        // The first-wake investigation caught the killer HERE — the first stop
        // of a VP-armed engine lapses the session's activation — and had to
        // reconstruct it from system noise because nothing named the moment.
        // One line, before the guard, so even a no-op stop says it was asked.
        log.info("engine stop: disposition=\(String(describing: session), privacy: .public) wasRunning=\(self.isRunning, privacy: .public) aec=\(self.echoCancellationActive, privacy: .public) appState=\(UIApplication.shared.applicationState.rawValue, privacy: .public)")
        guard isRunning else { return }
        stopLevelTimer()
        engine.inputNode.removeTap(onBus: 0)
        player.stop()
        engine.stop()
        isRunning = false
        interrupted = false
        level = 0
        levelValue.withLock { $0 = 0 }
        // `player.stop()` above flushed the queued audio, so the drain deadline
        // computed for it is now a claim about silence. Reset it, or the next
        // call within that window opens with a live microphone gated against
        // audio that will never play — the inverse of the echo bug.
        playbackClock.withLock { $0.reset() }
        captureGate.withLock { $0 = VoiceCaptureGateState() }
        releaseSessionIfRequested(session)
    }

    /// What the call needs the shared session to be: `.playAndRecord` / `.voiceChat`,
    /// output defaulted to the speaker, capture following a connected headset.
    ///
    /// `.voiceChat` is the mode that arms the hardware voice-processing I/O, so it is
    /// required rather than preferred — without it the microphone re-captures the
    /// speaker and the model answers its own reply.
    ///
    /// Lifted to `nonisolated static let` for one reason: **`AmbientMicEngine`
    /// configures the session with the same triple, and their equality is what makes
    /// the wake handoff a no-op rather than a re-configuration.** That equality is
    /// asserted in `AmbientMicEngineTests`. Before it held, the handoff changed the
    /// mode from the background and the armed window lost its microphone. Change
    /// either side and that test fails, which is the whole point of naming them.
    nonisolated static let callSessionCategory: AVAudioSession.Category = .playAndRecord
    nonisolated static let callSessionMode: AVAudioSession.Mode = .voiceChat
    nonisolated static let callSessionOptions: AVAudioSession.CategoryOptions = [
        .defaultToSpeaker, .allowBluetoothHFP,
    ]

    /// The transport's rate, stated as a hardware *preference* on a `.release` start.
    ///
    /// Deliberately NOT stated on a `.keepActive` start, and not because it would be
    /// refused: `setPreferredSampleRate` is a hint that takes effect at the next
    /// activation, and re-stating it mid-window would bias the hardware under a tap
    /// the armed window owns. Nothing needs it — the capture tap resamples from
    /// whatever `outputFormat(forBus:)` reports to `targetFormat`, on both paths.
    nonisolated static let preferredHardwareSampleRate: Double = 24_000

    /// Configure and activate the shared session — **only when this call owns it.**
    ///
    /// The three lines inside the guard are the three that broke ambient mode on a
    /// device: a `setCategory` that changed the session's *mode*, a
    /// `setPreferredSampleRate`, and a `setActive(true)` — reached from the background
    /// by the wake handoff, against a session `AmbientMicEngine` had activated in the
    /// foreground and must be left alone (Apple DTS 826462). A repeat activation of an
    /// active session was argued to be a harmless no-op; the mode change and the
    /// voice-processing rebuild layered on top of it were not, and the observed result
    /// was a degenerate input format and a disarm reading "Lost the microphone."
    ///
    /// **The skip is not a "session already configured?" check, and must not become
    /// one.** It asks who owns the session, which is a fact the call site knows and
    /// `AVAudioSession` cannot be asked (there is no API that reports whether a
    /// session is active, and `category`/`mode` read back what was *requested*). An
    /// idempotence check would also be wrong in principle: the point is not that the
    /// work is redundant, it is that this call is not entitled to do it.
    ///
    /// `internal` rather than `private`, and counted, for exactly the reason
    /// `releaseSessionIfRequested` is: `start`'s remaining body cannot run in the
    /// simulator (`AVAudioEngine` capture has no input), so a test driven through
    /// `start` alone would observe the *request* and never whether the session was
    /// touched. See `sessionConfigureCount`.
    func configureSharedSessionIfOwned(_ disposition: VoiceSessionDisposition) throws {
        guard disposition.configuresSharedSession else { return }
        sessionConfigureCount += 1
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(
            Self.callSessionCategory,
            mode: Self.callSessionMode,
            options: Self.callSessionOptions
        )
        try session.setPreferredSampleRate(Self.preferredHardwareSampleRate)
        try session.setActive(true)
    }

    /// The one executable deactivation in this file.
    ///
    /// `internal` rather than `private`, and a separate method rather than two
    /// inline lines, so the decision can be exercised directly: `stop()`'s
    /// `isRunning` guard cannot be satisfied in the simulator (the engine needs a
    /// microphone to start), so a test driven through `stop()` alone can only ever
    /// observe the *request*, never whether the session actually survived it.
    func releaseSessionIfRequested(_ disposition: VoiceSessionDisposition) {
        guard disposition == .release else { return }
        do {
            try AVAudioSession.sharedInstance().setActive(
                false,
                options: .notifyOthersOnDeactivation
            )
            sessionReleaseCount += 1
        } catch {
            // A busy session refuses deactivation — another call's IO is
            // riding it — and a refused deactivation is NOT a deactivation:
            // the counter answers "did this engine actually take the session
            // down", so it must not move on a refusal. Still non-fatal, same
            // as the swallowed `try?` this replaces: the owner keeps its
            // session, which is exactly what busy means.
        }
    }

    // MARK: - Level publishing

    /// Publish the render thread's latest RMS to `@Published level` at ~20 Hz, so
    /// the render thread never touches the main-actor property (it writes a lock).
    private func startLevelTimer() {
        stopLevelTimer()
        let timer = Timer(timeInterval: 1.0 / 20.0, repeats: true) { [weak self] _ in
            guard let self else { return }
            let value = self.levelValue.withLock { $0 }
            Task { @MainActor in self.level = value }
        }
        RunLoop.main.add(timer, forMode: .common)
        levelTimer = timer
    }

    private func stopLevelTimer() {
        levelTimer?.invalidate()
        levelTimer = nil
    }

    // MARK: - Capture

    /// Runs on the **audio render thread**. `nonisolated` so it legally touches
    /// only render-thread-confined state (`converter`/`conversionBuffer`, single-
    /// threaded) and lock-guarded cross-thread state (`mutedFlag`, `levelValue`) —
    /// never `@MainActor` properties. No `Task` spawn, no intermediate `Array`.
    nonisolated private func processCapture(_ buffer: AVAudioPCMBuffer,
                                            onFrameOut: (Data) -> Void) {
        let inputFormat = buffer.format
        // Build the converter once + only on a route/format change (not per buffer).
        if converter == nil || converterInputFormat != inputFormat {
            converter = AVAudioConverter(from: inputFormat, to: targetFormat)
            converterInputFormat = inputFormat
            conversionBuffer = nil   // capacity depends on the ratio; rebuilt below
        }
        guard let converter else { return }

        // Reuse the output buffer across calls (grow only if a bigger frame arrives).
        let ratio = targetFormat.sampleRate / inputFormat.sampleRate
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 1024
        if conversionBuffer == nil || conversionBuffer!.frameCapacity < capacity {
            conversionBuffer = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: capacity)
        }
        guard let out = conversionBuffer else { return }
        out.frameLength = 0

        var fed = false
        var conversionError: NSError?
        let status = converter.convert(to: out, error: &conversionError) { _, outStatus in
            // Supply the source buffer exactly once; then signal end-of-stream
            // so the converter drains rather than blocking for more input.
            if fed {
                outStatus.pointee = .noDataNow
                return nil
            }
            fed = true
            outStatus.pointee = .haveData
            return buffer
        }

        guard status != .error, conversionError == nil,
              let channel = out.floatChannelData, out.frameLength > 0 else { return }

        let count = Int(out.frameLength)
        let ptr = channel[0]

        // RMS over the resampled samples straight from the pointer (no Array alloc).
        var sumSquares: Float = 0
        for i in 0..<count { let s = ptr[i]; sumSquares += s * s }
        let rms = count > 0 ? (sumSquares / Float(count)).squareRoot() : 0
        levelValue.withLock { $0 = min(1, rms) }   // the level timer publishes this

        guard !muted else { return }
        // "Speaking" is judged against when the queued audio DRAINS, not when
        // frames arrived — see `playbackClock` for the fallacy this replaces.
        let drainsAt = playbackClock.withLock { $0.idleAtUptime }
        let playbackStarted = drainsAt > 0
        let now = CACurrentMediaTime()
        let assistantSpeaking = VoiceCaptureGateState.playbackAudible(
            drainsAt: drainsAt, tail: Self.halfDuplexHangover, now: now
        )
        let initialPlaybackActive = VoiceCaptureGateState.playbackAudible(
            drainsAt: drainsAt, tail: Self.initialPlaybackHangover, now: now
        )
        let aecActive = !halfDuplexFallback.withLock { $0 }
        let suppressCapture = captureGate.withLock { state in
            state.shouldSuppressCapture(
                playbackStarted: playbackStarted,
                initialPlaybackActive: initialPlaybackActive,
                assistantSpeaking: assistantSpeaking,
                echoCancellationActive: aecActive
            )
        }
        // The level meter above still updates while capture is suppressed, so
        // the voice beacon remains responsive.
        if suppressCapture { return }
        onFrameOut(VoicePCM.floatToInt16LE(UnsafeBufferPointer(start: ptr, count: count)))
    }

    // MARK: - Interruptions

    @objc private func handleInterruption(_ notification: Notification) {
        guard
            let info = notification.userInfo,
            let raw = info[AVAudioSessionInterruptionTypeKey] as? UInt,
            let type = AVAudioSession.InterruptionType(rawValue: raw)
        else { return }

        Task { @MainActor [weak self] in
            guard let self else { return }
            // The investigation's blind spot, closed: no interruption was
            // logged anywhere, so a session the SYSTEM took could not be told
            // apart from an activation our own stop lapsed. One line, no
            // behaviour change. Reason arrives only on iOS 14.5+ payloads and
            // options only on `.ended`; absent keys print as nil rather than
            // being guessed.
            self.log.info("audio session interruption: type=\(raw, privacy: .public) reason=\(String(describing: info[AVAudioSessionInterruptionReasonKey] as? UInt), privacy: .public) options=\(String(describing: info[AVAudioSessionInterruptionOptionKey] as? UInt), privacy: .public) appState=\(UIApplication.shared.applicationState.rawValue, privacy: .public)")
            switch type {
            case .began:
                // A phone call / Siri / another app grabbed the session.
                // `pause()` keeps the queue, so the playback clock's drain
                // deadline is deliberately NOT reset here: clearing it would
                // open the half-duplex gate into whatever remainder of the
                // reply resumes with `.shouldResume`. Only flushes reset.
                // The mirror-image staleness is ACCEPTED, not overlooked:
                // while paused the deadline keeps spending wall time, so a
                // resumed remainder can outlive it and sound past a reopened
                // half-duplex gate. Reaching that takes the fallback (no AEC)
                // + an interruption + a resume in one call, and the ambient
                // path ends its window on interruption rather than resuming —
                // narrow enough to carry rather than complicate the clock for.
                self.player.pause()
                self.interrupted = true
            case .ended:
                guard self.interrupted else { return }
                self.interrupted = false
                let options: AVAudioSession.InterruptionOptions
                if let rawOpts = info[AVAudioSessionInterruptionOptionKey] as? UInt {
                    options = AVAudioSession.InterruptionOptions(rawValue: rawOpts)
                } else {
                    options = []
                }
                if options.contains(.shouldResume), self.isRunning {
                    // **This reactivation cannot succeed in the background**, which
                    // is precisely where an armed ambient window lives: iOS refuses
                    // to activate a recording session from a non-foreground app
                    // (DTS 826462 — see `VoiceSessionDisposition`). It is kept, and
                    // deliberately kept `try?`, because for the in-app call the app
                    // *is* on screen and this is the line that brings audio back
                    // after a phone call or Siri. For an ambient call it is a
                    // no-op that fails silently — and it has to stay a no-op:
                    // the ambient answer to an interruption is to end the window
                    // rather than resume it (`AmbientMicEngine.interruptionOutcome`
                    // maps `.ended` to `.ignore` for exactly this reason), so
                    // nothing here should grow into a retry or a fallback.
                    try? AVAudioSession.sharedInstance().setActive(true)
                    self.player.play()
                }
            @unknown default:
                break
            }
        }
    }
}
