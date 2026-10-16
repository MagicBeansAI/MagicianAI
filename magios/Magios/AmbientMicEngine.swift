// `@preconcurrency` for the same reason `VoiceAudioEngine` needs it: AVFAudio's
// `AVAudioPCMBuffer` is not `Sendable`, but the capture buffer handed back to the
// converter's `@Sendable` input block never crosses an isolation boundary (the
// converter calls that block synchronously). Silences a spurious warning without
// changing behaviour.
@preconcurrency import AVFoundation
import Foundation
import os
// `UIApplication.didBecomeActiveNotification` — the only moment a microphone
// permission the user revoked in Settings can be noticed. See `AmbientMicEngine`.
import UIKit

/// Why an armed window's microphone died, or refused to open.
///
/// **The controller does not read this**: `AmbientController` routes `onFailure`
/// to `disarm(reason:)` with one generic caption, because ambient mode is for
/// someone who is not looking at their phone and "Lost the microphone." is worth
/// more to that person than a route-change reason code. The cases exist so the
/// log says which failure happened and so the pure decision functions below can
/// be asserted on. Do not widen the orb to show them.
enum AmbientMicFailure: Error, Equatable {
    /// The record permission is not granted, so no tap can be opened at all.
    case recordPermissionMissing
    /// An incoming call, Siri, or another app took the session.
    case interrupted
    /// The route changed and left nothing to capture from.
    case inputRouteLost
    /// The media server restarted. Every audio object in the process is invalid.
    case mediaServicesReset
    /// The input node reported a format with no sample rate or no channels, which
    /// `AVAudioEngine.installTap` answers with a raised `NSException` rather than an
    /// error. Reached when the session is inactive or the route has momentarily no
    /// input — see `AmbientMicEngine.installTap`.
    case inputFormatInvalid
    /// A route changed and the tap could not be reinstalled against the new format.
    case tapRebuildFailed
}

/// What a session / route / lifecycle notification means for the armed tap.
///
/// A separate type because the mapping is the only part of the failure path that
/// can be tested at all — `AVAudioEngine` microphone capture has no input in the
/// simulator — and because `.ignore` is the answer that carries the risk. A
/// notification wrongly mapped to `.fail` ends a window the user was still using;
/// one wrongly mapped to `.ignore` leaves the orb claiming the user is being heard
/// by a tap that is dead.
enum AmbientMicOutcome: Equatable {
    case ignore
    case fail(AmbientMicFailure)
}

/// The tap thread's resampler: whatever format the hardware input runs at →
/// 16 kHz mono PCM16-LE bytes, ready for `WakeSpotter.feed` — since the
/// 2026-07-30 decision removed pre-ready capture, the spotter is the frames'
/// only consumer.
///
/// **Its own type because it is the only part of the capture path that runs
/// without hardware**, and therefore the only part that can be exercised in the
/// simulator: `AVAudioConverter` is software, so a synthesised input buffer
/// proves the sample rate, the channel count and the byte parity the PCM16
/// contract requires (whole samples, never a split one — the property
/// `WakePreRoll.append` used to `precondition` on, and still worth holding for
/// any PCM16 consumer). Left inline in the engine it would only ever have been
/// verified by hand on a device.
///
/// **Confined to one tap.** No locking, and none is needed *because* the engine
/// constructs a fresh one per `installTap` and the tap closure owns it: the only
/// caller is that one tap's callback, and `AVAudioEngine` serialises those. Shared
/// across taps it would not be safe — a `stop()` cannot guarantee the previous
/// callback has left `encode`, so a stop→start pair could put two render threads in
/// here at once. Do not hoist it to a property to "reuse the converter"; the
/// converter is rebuilt on a format change anyway, which is what a new tap usually
/// means.
final class AmbientMicResampler {

    /// What the wake spotter is specified in: 16 kHz mono. The engine converts
    /// to float32 here and encodes to PCM16-LE on the way out, exactly as
    /// `ListenController` and `VoiceAudioEngine` do.
    static let targetFormat = AVAudioFormat(
        commonFormat: .pcmFormatFloat32,
        sampleRate: 16_000,
        channels: 1,
        interleaved: false
    )!

    private var converter: AVAudioConverter?
    private var converterInputFormat: AVAudioFormat?
    private var output: AVAudioPCMBuffer?

    /// Output capacity for one input buffer, with headroom.
    ///
    /// Pure and `static` so the arithmetic is assertable: a sample-rate converter
    /// can emit slightly more than the naive ratio suggests, and a buffer sized to
    /// the exact ratio silently truncates. The `+ 1024` is the same headroom both
    /// existing capture paths use. It also keeps the capacity non-zero for a very
    /// short input buffer, where `Double(frames) * ratio` rounds to 0 and
    /// `AVAudioPCMBuffer(pcmFormat:frameCapacity: 0)` returns nil.
    static func capacity(
        inputFrames: AVAudioFrameCount,
        inputRate: Double,
        targetRate: Double
    ) -> AVAudioFrameCount {
        guard inputRate > 0 else { return 1024 }
        let ratio = targetRate / inputRate
        return AVAudioFrameCount(Double(inputFrames) * ratio) + 1024
    }

    /// 16 kHz mono PCM16-LE for one captured buffer, or nil if there was nothing
    /// to convert.
    ///
    /// **The returned byte count is always even**, because `VoicePCM.floatToInt16LE`
    /// allocates `samples * 2` and writes whole `Int16`s — whole PCM16 samples,
    /// never a split one, for whatever consumes the frame. There is deliberately
    /// no path here that slices, pads or truncates the encoded bytes; adding one
    /// would reintroduce the odd-count door this closes.
    func encode(_ buffer: AVAudioPCMBuffer) -> Data? {
        guard buffer.frameLength > 0 else { return nil }
        let inputFormat = buffer.format
        // Built on the first buffer, and rebuilt if the format ever differs.
        //
        // **The rebuild branch is unreachable in production, and is kept as a
        // defensive invariant rather than as a recovery path.** One resampler
        // belongs to one tap, and `installTap(onBus:bufferSize:format:)` fixes the
        // buffer format for that tap's lifetime, so `converterInputFormat` cannot
        // change underneath it. Recovery from a headset being connected or
        // disconnected mid-window happens a level up: `AVAudioEngine` posts a
        // configuration change, and `rebuildTap` reinstalls at the new format with
        // a NEW resampler. (An earlier draft shared one resampler across taps, and
        // then this branch really was the recovery; it is not any more.)
        //
        // Keeping it costs one comparison per buffer and means this type carries no
        // assumption about a format it was never told to expect —
        // `AmbientMicEngineTests` pins that property directly.
        if converter == nil || converterInputFormat != inputFormat {
            converter = AVAudioConverter(from: inputFormat, to: Self.targetFormat)
            converterInputFormat = inputFormat
            output = nil  // capacity depends on the ratio; rebuilt below
        }
        guard let converter else { return nil }

        let capacity = Self.capacity(
            inputFrames: buffer.frameLength,
            inputRate: inputFormat.sampleRate,
            targetRate: Self.targetFormat.sampleRate
        )
        if output == nil || output!.frameCapacity < capacity {
            output = AVAudioPCMBuffer(pcmFormat: Self.targetFormat, frameCapacity: capacity)
        }
        guard let out = output else { return nil }
        out.frameLength = 0

        var fed = false
        var conversionError: NSError?
        let status = converter.convert(to: out, error: &conversionError) { _, outStatus in
            // Supply the source buffer exactly once, then signal end-of-input so
            // the converter drains what it has rather than waiting for more.
            if fed {
                outStatus.pointee = .noDataNow
                return nil
            }
            fed = true
            outStatus.pointee = .haveData
            return buffer
        }
        guard status != .error, conversionError == nil,
              out.frameLength > 0, let channel = out.floatChannelData
        else { return nil }
        return VoicePCM.floatToInt16LE(
            UnsafeBufferPointer(start: channel[0], count: Int(out.frameLength))
        )
    }
}

/// The real armed-window microphone tap: `AVAudioEngine` input → 16 kHz mono
/// PCM16 frames handed to one callback, and nothing else.
///
/// ## The audio session is activated only in the FOREGROUND, and NEVER deactivated
///
/// **This is the single most important property of this type, and half of it is
/// invisible in the code, because it is an absence.** There is exactly one
/// executable `setActive(` in this file — inside `activateSessionInForeground` —
/// and exactly zero deactivations. Both counts are load-bearing. (A grep has to
/// strip comments to see that: the string `setActive(false)` appears several times
/// below, every one of them explaining why there are none.)
///
/// Apple DTS (thread 826462, May 2026) states the whole rule as a recipe: have the
/// `audio` background mode, and *only activate your audio session in the
/// foreground*. Activate while visible and never deactivate, and the **engine** can
/// then be started and stopped from the background indefinitely. Deactivate it and
/// it cannot be reactivated from the background — at which point the armed window
/// dies silently at the first wake word, which is the one moment nobody is watching
/// a screen to notice.
///
/// So `stop()` stops the *engine* and removes the *tap*, and leaves the *session*
/// active. It looks unfinished next to `ListenController.stopEngine()`, which ends
/// with `setActive(false, .notifyOthersOnDeactivation)`, and next to
/// `VoiceAudioEngine.stop()`, which does the same. **Do not "finish" it.** If you
/// are here to add the missing deactivation, that is the bug — see
/// `docs/components/magios/ambient-mode.md`.
///
/// `.mixWithOthers` is deliberately absent for a different and less settled
/// reason: two Apple DTS threads contradict each other on whether a mixable
/// session can be activated from the background at all. `BackgroundEngine` uses
/// `.playback` + `.mixWithOthers` for an unrelated purpose (not pausing the user's
/// music behind a task keepalive), and that precedent must not be copied here
/// while the question is open. The design's §15 records the contradiction.
///
/// ## It follows the user's headset, unlike its neighbour
///
/// `ListenController` deliberately omits `.allowBluetoothHFP` so that a phone left
/// on a table captures the whole room rather than a near-field HFP headset mic.
/// Ambient mode wants the opposite and says so explicitly at the call site: the
/// user is wearing something, talking to an assistant, and the reply is spoken
/// back. See `configureSession`.
///
/// ## No networking collaborator
///
/// **There is nothing here that can reach the network**, and that absence is the
/// feature's privacy guarantee rather than an omission: while armed, the only
/// destination for captured audio is the on-device wake spotter — the pre-roll
/// ring that used to sit beside it went with the 2026-07-30 no-pre-ready-capture
/// decision. No pump, no accumulator, no client, no upload — compare
/// `ListenController`, which exists to push audio at a backend. Same rule, and the
/// same reason, as `WakeSpotter`.
///
/// ## Device-only
///
/// `AVAudioEngine` microphone capture has no input in the simulator, so `start`,
/// `stop` and the tap callback cannot be exercised there. What *is* tested is
/// everything factored out of them: the resampler above, the capacity arithmetic,
/// the session-configuration triple, and the notification → outcome mapping below.
/// The rest is `Task 13`'s, and the list of what it owes is in the doc.
@MainActor
final class AmbientMicEngine: AmbientMicSource {

    /// 4096 input frames ≈ 85 ms at 48 kHz — the same size `ListenController`
    /// taps at. Wake latency is dominated by Vosk's ~1600 ms endpointing, not by
    /// this, so the larger buffer's fewer wakeups are worth more than the frame
    /// granularity across a window that can stay armed for many minutes.
    private static let tapBufferSize: AVAudioFrameCount = 4096

    private let log = Logger(subsystem: "ai.magicbeans.magios", category: "ambient.mic")

    /// The frame callback, behind the lock that makes `stop()`'s contract true.
    ///
    /// `@unchecked Sendable` because `AmbientMicSource.onFrame` is deliberately a
    /// plain non-`Sendable` closure (it is called on the audio thread), and this
    /// lock is precisely the synchronisation the annotation stands in for.
    private struct FrameSink: @unchecked Sendable {
        var onFrame: ((Data) -> Void)?
    }

    /// **The whole of the no-callback-in-flight guarantee.**
    ///
    /// The tap delivers only from inside `withLock`, and `stop()` nils the closure
    /// from inside `withLock`. So a delivery already running finishes before
    /// `stop()` returns, and one that arrives afterwards finds nil — which is what
    /// `AmbientMicSource.stop()` requires and what `removeTap`/`engine.stop()`
    /// does not provide on its own. The guarantee earned its keep against the
    /// pre-roll ring's drain (a late frame appending during it was an
    /// exclusivity trap at the wake moment); the ring went with the 2026-07-30
    /// decision, and the guarantee stays — a frame delivered after `stop()`
    /// returns would feed a spotter the handoff has already abandoned, and the
    /// `AmbientMicSource` contract promises no such delivery.
    private let sink = OSAllocatedUnfairLock(initialState: FrameSink())

    /// The mid-window failure channel. Main-actor state, and nil whenever no tap
    /// is running, which is what stops a queued hop from reporting a failure for a
    /// window that has already been torn down.
    ///
    /// **Nilling it is necessary but NOT sufficient**, which is why `generation`
    /// exists as well: `start` re-arms this flag, so on its own it cannot tell "my
    /// window" from "the window that replaced mine". See `generation`.
    private var onFailure: (@MainActor (Error) -> Void)?

    /// Which tap a queued notification belongs to.
    ///
    /// **Because `isRunning` and `onFailure` cannot distinguish "still my window"
    /// from "a later one".** Both are re-armed by `start`, so a notification hop
    /// queued during tap A and drained after tap B has started finds `isRunning`
    /// true and B's handler installed, and reports A's failure against B — which
    /// the controller answers by disarming a perfectly healthy window.
    ///
    /// It is reachable without anything unusual: `handoff` separates `mic.stop()`
    /// from `resumeSpotting`'s `mic.start` by exactly one suspension point, and a
    /// call sink that fails *without* suspending never yields the main actor, so
    /// the stop/start pair completes inside one turn while the hop is still queued
    /// behind it.
    ///
    /// Bumped by BOTH `start` and `stop`, so a hop cannot survive either edge. In a
    /// lock rather than as main-actor state because the `@objc` handlers have to
    /// read it *before* they hop — reading it after would sample the value the
    /// comparison exists to detect. Same generation idiom `AmbientController` uses
    /// for its own windows (`armedAt`), for the same reason: a state-shaped check
    /// cannot tell two windows apart.
    private let generation = OSAllocatedUnfairLock<UInt64>(initialState: 0)

    private var engine: AVAudioEngine?
    private var isRunning = false

    init() {
        let center = NotificationCenter.default
        center.addObserver(
            self,
            selector: #selector(handleInterruption(_:)),
            name: AVAudioSession.interruptionNotification,
            object: nil
        )
        center.addObserver(
            self,
            selector: #selector(handleRouteChange(_:)),
            name: AVAudioSession.routeChangeNotification,
            object: nil
        )
        center.addObserver(
            self,
            selector: #selector(handleMediaServicesReset(_:)),
            name: AVAudioSession.mediaServicesWereResetNotification,
            object: nil
        )
        center.addObserver(
            self,
            selector: #selector(handleDidBecomeActive(_:)),
            name: UIApplication.didBecomeActiveNotification,
            object: nil
        )
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }

    // MARK: - AmbientMicSource

    func start(
        onFrame: @escaping (Data) -> Void,
        onFailure: @escaping @MainActor (Error) -> Void
    ) throws {
        // A second `start` without a `stop` would install a second tap on the same
        // bus, which is an ObjC exception rather than a Swift error.
        guard !isRunning else { return }
        // Checked before anything is configured so a denied microphone surfaces as
        // `arm`'s "Couldn't open the microphone." rather than as an armed window
        // with a silent tap.
        guard AVAudioApplication.shared.recordPermission == .granted else {
            throw AmbientMicFailure.recordPermissionMissing
        }
        let session = AVAudioSession.sharedInstance()
        try configureSession(session)
        try activateSessionInForeground(session)

        let engine = AVAudioEngine()
        // Installed before the engine starts so no captured audio is dropped on
        // the floor between the first buffer and the closure being visible.
        sink.withLock { $0.onFrame = onFrame }
        self.onFailure = onFailure
        // Bumped BEFORE the tap exists, not after it succeeds, and the ordering is
        // the whole point. A notification posted while `installTap`/`engine.start()`
        // runs is stamped by its handler with whatever generation is current at that
        // instant; if this bump came afterwards, that stamp would be the OLD window's
        // and the hop would be discarded — throwing away a failure that is genuinely
        // true for the window arming right now. That is the false-negative direction:
        // an armed orb over a dead tap, which is worse than the wasted window a
        // false positive costs. Bumping first stamps such a notification with THIS
        // window, so it is honoured.
        //
        // It weakens nothing. `start` is synchronous, so a hop cannot observe
        // `isRunning` until this method returns, and the previous window's older
        // stamp is invalidated either way. A `start` that then throws leaves the
        // counter advanced with nothing running, which is harmless — it only
        // discards hops older than a window that failed to open, and `report`'s
        // `isRunning`/`onFailure` guards cover the rest.
        generation.withLock { $0 += 1 }
        do {
            try installTap(on: engine)
        } catch {
            // Exception-safe: leave nothing half-armed for `stop()` to miss.
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
            sink.withLock { $0.onFrame = nil }
            self.onFailure = nil
            throw error
        }
        self.engine = engine
        isRunning = true
        // Scoped to OUR engine instance rather than `object: nil`, because
        // `VoiceAudioEngine` and `BackgroundEngine` post the same notification and
        // rebuilding our tap because someone else's graph changed is wasted work at
        // best and a spurious failure at worst. Registered here rather than in
        // `init` for the same reason: the object to scope to does not exist until
        // now, and `stop()` removes it again.
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleConfigurationChange(_:)),
            name: .AVAudioEngineConfigurationChange,
            object: engine
        )
    }

    /// Stop the ENGINE and remove the TAP. **Leaves the session active** — see the
    /// type comment before changing anything here.
    ///
    /// Safe with nothing running, which is the common path rather than an edge
    /// case: `AmbientController.disarm` calls it on every disarm, including from
    /// `.armed` windows whose tap was already released by the wake handoff.
    func stop() {
        // First, and under the lock: this is what makes the no-callback-in-flight
        // guarantee true for `onFrame`. Everything below is bookkeeping.
        sink.withLock { $0.onFrame = nil }
        // Half of the same guarantee for `onFailure`: no lock is needed because
        // every report path is main-actor and therefore cannot interleave with this
        // method, only arrive after it and find nil. It covers a hop that lands
        // while nothing is armed; the generation below is what covers one that lands
        // after a NEW window armed, which this nil cannot see.
        onFailure = nil
        // Bumped unconditionally, including when nothing was running: the point is
        // to invalidate whatever hops are still queued, and a stop that found no
        // tap is exactly when there are stale ones about. See `generation`.
        generation.withLock { $0 += 1 }
        if let engine {
            // Scoped to this engine, so it has to be removed before the reference
            // goes. The other four observers are process-wide and outlive the tap;
            // what disarms them is the generation, plus `report`'s `isRunning`.
            NotificationCenter.default.removeObserver(
                self,
                name: .AVAudioEngineConfigurationChange,
                object: engine
            )
        }
        engine?.inputNode.removeTap(onBus: 0)
        engine?.stop()
        engine = nil
        isRunning = false
        // There is nothing here to reset the resampler, and that is not an
        // oversight: it is owned by the tap closure rather than by this object (see
        // `installTap`), so it goes when the tap does. Which is what makes its
        // lock-free "one serialised caller" justification literally true — a
        // callback can still be inside `encode` at this instant, and the next
        // `start` builds a new engine with a new resampler rather than handing a
        // second render thread the same one.
        //
        // And NO `AVAudioSession.setActive(false)`. Not an omission.
    }

    /// Re-prime the existing input graph after sequential TTS playback.
    ///
    /// On physical iPhones `AVSpeechSynthesizer` can finish with
    /// `AVAudioEngine.isRunning == true` while the input render callback no
    /// longer advances. Merely reopening a higher-level PCM gate then creates a
    /// convincing but deaf follow-up window. Reinstalling the tap and starting
    /// the same engine forces the AudioUnit back through StartIO without
    /// allocating a new recorder or touching the already-active shared
    /// `AVAudioSession`.
    ///
    /// Rebuild the retained tap against the current hardware format every time.
    /// `AVAudioEngine.start()` can succeed while retaining the post-playback dead
    /// input callback, so a pause/start fast path is not a valid recovery signal.
    /// Failure leaves the source fully stopped so the caller's bounded retry rail
    /// has one truthful state to recover from.
    func refreshAfterPlayback() throws {
        guard isRunning, let engine else {
            throw AmbientMicFailure.tapRebuildFailed
        }
        generation.withLock { $0 += 1 }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        do {
            try installTap(on: engine)
            log.info("Rebuilt the ambient input graph after playback.")
        } catch {
            stop()
            throw error
        }
    }

    // MARK: - Session

    /// `.playAndRecord` / `.voiceChat`, output defaulted to the speaker so the
    /// spoken reply is audible with the phone face-up on a table.
    ///
    /// **The mode is `.voiceChat` because it is the mode the CONVERSATION needs,
    /// and the armed window is the only party allowed to choose it.** It was
    /// `.spokenAudio` — the neighbour's choice, right for a spotting tap in
    /// isolation — and that one word was this feature's headline failure on a
    /// device: the wake phrase was heard while backgrounded, and then the
    /// microphone was lost. `VoiceAudioEngine` needed `.voiceChat` for the hardware
    /// echo canceller, so the wake handoff performed a **mode change plus a
    /// re-activation** on a session that was activated in the foreground and had to
    /// stay untouched (DTS 826462 — activating from the background is refused, and
    /// a mode swap with a voice-processing rebuild is not "leaving an active session
    /// alone"). The input node came back degenerate, `installTap`'s format guard
    /// refused it, and `resumeSpotting` disarmed with "Lost the microphone."
    ///
    /// So the session is configured **once, at arm time, in the foreground, with the
    /// mode the conversation needs**, and the conversation path does not touch it
    /// at all: `VoiceAudioEngine.configureSharedSessionIfOwned` skips `setCategory`,
    /// `setPreferredSampleRate` and `setActive` for a `.keepActive` caller. The two
    /// triples are asserted equal in `AmbientMicEngineTests` — that equality is what
    /// makes "the handoff requires no session change" a checked property rather than
    /// a coincidence, and it is the invariant to preserve if either side is edited.
    ///
    /// **Accepted cost, and it is a real one: `.voiceChat` arms the system AEC/AGC
    /// for the SPOTTING tap too**, which is not a rename. The wake spotter now sees
    /// processed rather than raw audio, and the mode typically forces a different
    /// input sample rate. The rate is handled by construction — nothing here states a
    /// preferred rate, `installTap` taps at whatever `outputFormat(forBus:)` reports,
    /// and `AmbientMicResampler` rebuilds its converter on any input-format change —
    /// but whether wake ACCURACY holds under AEC/AGC is a device question and is on
    /// the checklist in `docs/components/magios/ambient-mode.md`.
    ///
    /// **`.allowBluetoothHFP` is deliberate and is the opposite of the neighbour's
    /// choice.** `ListenController` omits it so that a phone on a table hears the
    /// room instead of switching to a near-field HFP headset microphone — for an
    /// observation session that is right, and it is the *omission* that implements
    /// it, not any explicit pinning. Ambient mode has the inverse requirement: the
    /// user is having a hands-free conversation, so capture should follow whatever
    /// they are wearing, and a headset that hears the wake word is the whole point.
    /// Without this option a connected Bluetooth headset's microphone is not
    /// offered as an input at all, so its absence would silently pin ambient to the
    /// built-in mics. Both `DictationController` and `VoiceAudioEngine` — the two
    /// paths that also exist to hear one person talking — set the same pair.
    ///
    /// Re-applied on every `start`, and it is no longer the wake handoff that makes
    /// that necessary — the conversation leaves the session exactly as it found it.
    /// What still can change it out from under an armed window is a *neighbour* that
    /// legitimately yields the window first (`ListenController.startEngine`,
    /// `DictationController.startLive`) and a `.playback` re-categorisation that
    /// refuses while a window is live (`SpeechSynthesizer.configureSession`); the
    /// next `arm` has to state the whole triple regardless. Setting a category is not
    /// activating a session, so this is safe from the background.
    ///
    /// The triple is lifted to `nonisolated static let` so it can be asserted
    /// without hardware. It is the decision in this file most likely to be
    /// "tidied" back toward `ListenController`'s — the same class of edit the
    /// never-deactivate rule needs three comments to survive — and unlike that one,
    /// this one is cheap to pin in a test. The mode has already been reverted once by
    /// exactly that reasoning, and the cost was the feature's headline bug.
    nonisolated static let sessionCategory: AVAudioSession.Category = .playAndRecord
    nonisolated static let sessionMode: AVAudioSession.Mode = .voiceChat
    nonisolated static let sessionOptions: AVAudioSession.CategoryOptions = [
        .defaultToSpeaker, .allowBluetoothHFP,
    ]

    private func configureSession(_ session: AVAudioSession) throws {
        try session.setCategory(Self.sessionCategory, mode: Self.sessionMode, options: Self.sessionOptions)
    }

    /// Activate the shared session — **in the foreground, and never undone.**
    ///
    /// The gate is the app's own state, deliberately, and **not** a "have I done
    /// this already?" latch. iOS refuses to activate a recording session from the
    /// background, so `resumeSpotting`'s `start` after a call — which routinely
    /// runs with the app off screen — must not attempt it; but the *foreground* is
    /// where activation is both legal and required, every time, because the session
    /// can have been deactivated since the last one by something that is not this
    /// file. The system deactivates it for the duration of an interruption; a
    /// media-services reset invalidates it; `DictationController` and
    /// `VoiceAudioEngine` deactivate it outright. A once-only latch would record
    /// that this object had *called* `setActive(true)` — a different proposition
    /// from the session being active — and then skip the call that would have put
    /// it back, permanently, for the rest of the process.
    ///
    /// This is also closer to what the DTS recipe actually says, which is *only
    /// activate in the foreground*, not *only activate once*. A repeat activation
    /// of an already-active session is a no-op; `DictationController` and
    /// `VoiceAudioEngine` both activate on every start for that reason.
    ///
    /// The counterpart rule has no code at all: nothing in this file deactivates.
    private func activateSessionInForeground(_ session: AVAudioSession) throws {
        guard UIApplication.shared.applicationState != .background else {
            // Not an error yet. If the session is still active — the normal case,
            // since nothing here deactivates — the engine starts and the window
            // continues. If it is not, `installTap` finds a degenerate input format
            // and throws, which `resumeSpotting` turns into an honest disarm rather
            // than a silent tap. Logged because that is the one path where the
            // reason is otherwise invisible.
            log.info("Skipping session activation: app is backgrounded (activation is foreground-only, DTS 826462).")
            return
        }
        try session.setActive(true)
    }

    // MARK: - Engine

    /// Install the tap at the route's current format and start the engine.
    ///
    /// **The format guard is the difference between a failure and a process
    /// crash.** `installTap` raises an uncatchable `NSException` — *"required
    /// condition is false: IsFormatSampleRateAndChannelCountValid(format)"* — when
    /// handed a format with a zero sample rate or channel count, which is exactly
    /// what `outputFormat(forBus:)` returns while the session is inactive or the
    /// route momentarily has no input. That is not a Swift error, so neither this
    /// function's `throws` nor `rebuildTap`'s `do`/`catch` would convert it into the
    /// failure path they were written for. Checking the format first is what routes
    /// the condition into `onFailure` instead.
    ///
    /// It matters more here than in `ListenController`, which reaches the same shape
    /// only from a foreground `start` immediately after `setActive(true)`. This
    /// engine has two riskier callers: `rebuildTap`, which fires from a
    /// configuration change, and `start` after a background activation was skipped.
    /// **An incoming call posts an interruption AND a configuration change as two
    /// unordered main-actor hops**, so if the configuration change wins, this runs
    /// against an interrupted, inactive session. And a crash does not take the orb
    /// down — a Live Activity lives in the system's process — so the Dynamic Island
    /// would go on saying "armed" with nothing listening until the next launch
    /// sweeps it. That is this feature's signature failure, reached by a raised
    /// ObjC exception.
    private func installTap(on engine: AVAudioEngine) throws {
        let input = engine.inputNode
        // Tap at the hardware's own format and resample inside the callback; the
        // input node runs at whatever the device and route dictate.
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else {
            log.error("Refusing to tap a degenerate input format (\(format.sampleRate, privacy: .public) Hz, \(format.channelCount, privacy: .public) ch).")
            throw AmbientMicFailure.inputFormatInvalid
        }
        // One resampler per tap, owned by the closure rather than by this object.
        // Its lack of locking is justified by `AVAudioEngine` serialising *one*
        // tap's callbacks — true within an engine, and every `start`/`rebuildTap`
        // builds a new engine, so a shared instance would let two render threads
        // into `encode` across a stop→start pair. `stop()` cannot rule that out; it
        // says so itself. Confining it here makes the justification literally true.
        let resampler = AmbientMicResampler()
        input.installTap(onBus: 0, bufferSize: Self.tapBufferSize, format: format) { [weak self] buffer, _ in
            guard let frame = resampler.encode(buffer) else { return }
            self?.deliver(frame)
        }
        engine.prepare()
        try engine.start()
    }

    /// Reinstall the tap at the route's new format after a configuration change.
    ///
    /// This is the *repair* half of following the user's headset: connecting or
    /// disconnecting one changes the input format, at which point `AVAudioEngine`
    /// has stopped and the old tap is describing a format the hardware no longer
    /// produces. Without this the window would stay `.armed` behind a dead tap —
    /// the orb lying about listening, which is the failure this feature is built
    /// to avoid — so a rebuild that cannot succeed reports a failure instead.
    ///
    /// **It touches the engine only.** No `setCategory`, no `setActive`: this runs
    /// in the background by definition (the user is plugging something in, not
    /// looking at the app), and an activation attempted here would fail and turn a
    /// recoverable route change into a dead window.
    private func rebuildTap() {
        guard isRunning, let engine else { return }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        do {
            try installTap(on: engine)
            log.info("Rebuilt the ambient tap after a configuration change.")
        } catch {
            log.error("Could not rebuild the ambient tap: \(error.localizedDescription, privacy: .public)")
            report(.tapRebuildFailed)
        }
    }

    /// Audio thread. **The only path from captured audio to anywhere**, and the
    /// only place the frame gate is taken — that singularity is what makes
    /// `stop()`'s guarantee checkable by reading one function. The absence of a
    /// second destination is the privacy invariant, the same way it is in
    /// `AmbientController.ingest`.
    private nonisolated func deliver(_ frame: Data) {
        sink.withLock { $0.onFrame?(frame) }
    }

    // MARK: - Failure reporting

    /// Close the tap, then report.
    ///
    /// In that order, because the controller's answer to a failure is `disarm`, and
    /// that is asynchronous: leaving the tap live in the meantime would keep
    /// feeding a spotter whose window is already being torn down. Stopping
    /// first also makes the report single-shot by construction — `stop()` nils
    /// `onFailure`, so a second notification arriving behind the first has nothing
    /// to deliver into.
    private func report(_ failure: AmbientMicFailure) {
        guard isRunning, let handler = onFailure else { return }
        log.error("Ambient tap failed: \(String(describing: failure), privacy: .public)")
        stop()
        handler(failure)
    }

    // MARK: - Notifications
    //
    // All five arrive on whatever thread posted them, so each one reads the current
    // `generation` and parses into a `Sendable` outcome with nonisolated code, then
    // hops to the main actor carrying nothing but those two values. The generation
    // is read BEFORE the hop on purpose: read after, it would sample the very value
    // the comparison exists to detect.

    @objc nonisolated private func handleInterruption(_ note: Notification) {
        let outcome = Self.interruptionOutcome(userInfo: note.userInfo)
        // Parsed for the log only, on the posting thread like the outcome —
        // the decision stays `interruptionOutcome`'s.
        let typeRaw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
        let reasonRaw = note.userInfo?[AVAudioSessionInterruptionReasonKey] as? UInt
        let optionsRaw = note.userInfo?[AVAudioSessionInterruptionOptionKey] as? UInt
        let observed = currentGeneration
        Task { @MainActor [weak self] in
            guard let self else { return }
            // The investigation's blind spot, closed: interruptions were acted
            // on but never recorded, so a session the SYSTEM took was
            // indistinguishable from an activation lapse of our own making.
            // One line, no behaviour change; absent keys print as nil.
            self.log.info("ambient tap interruption: type=\(String(describing: typeRaw), privacy: .public) reason=\(String(describing: reasonRaw), privacy: .public) options=\(String(describing: optionsRaw), privacy: .public) appState=\(UIApplication.shared.applicationState.rawValue, privacy: .public)")
            self.apply(outcome, from: observed)
        }
    }

    @objc nonisolated private func handleRouteChange(_ note: Notification) {
        // Parsed for the log only — the reason deliberately does not decide
        // anything. See `routeChangeOutcome`.
        let reason = note.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt ?? 0
        let observed = currentGeneration
        Task { @MainActor [weak self] in
            guard let self else { return }
            let hasInput = !AVAudioSession.sharedInstance().currentRoute.inputs.isEmpty
            let outcome = Self.routeChangeOutcome(hasInputRoute: hasInput)
            if case .fail = outcome {
                self.log.error("Route change (reason \(reason, privacy: .public)) left no input route.")
            }
            self.apply(outcome, from: observed)
        }
    }

    /// The route changed and `AVAudioEngine` has torn its graph down. Posted only
    /// for OUR engine — see the registration in `start`.
    @objc nonisolated private func handleConfigurationChange(_ note: Notification) {
        let observed = currentGeneration
        Task { @MainActor [weak self] in
            guard let self, self.isCurrent(observed) else { return }
            self.rebuildTap()
        }
    }

    @objc nonisolated private func handleMediaServicesReset(_ note: Notification) {
        // Every audio object in the process is invalid after this, including the
        // engine and the session's configuration. Rebuilding would mean
        // reactivating, which the background cannot do, so this is terminal.
        let observed = currentGeneration
        Task { @MainActor [weak self] in self?.apply(.fail(.mediaServicesReset), from: observed) }
    }

    /// **Deliberately not generation-checked.** Every other handler reports
    /// something that happened to a specific tap, so a later window must not inherit
    /// it. This one asks a fresh question at the moment of foregrounding — *is the
    /// microphone permission still granted for whatever tap is running now?* — and
    /// both halves of the answer are read after the hop. A generation comparison
    /// here would discard a legitimate check because an unrelated re-arm happened
    /// in between.
    @objc nonisolated private func handleDidBecomeActive(_ note: Notification) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            let granted = AVAudioApplication.shared.recordPermission == .granted
            guard case .fail(let failure) = Self.foregroundOutcome(
                isRunning: self.isRunning,
                permissionGranted: granted
            ) else { return }
            self.report(failure)
        }
    }

    private nonisolated var currentGeneration: UInt64 { generation.withLock { $0 } }

    private func isCurrent(_ observed: UInt64) -> Bool { currentGeneration == observed }

    private func apply(_ outcome: AmbientMicOutcome, from observed: UInt64) {
        guard isCurrent(observed) else { return }
        guard case .fail(let failure) = outcome else { return }
        report(failure)
    }

    // MARK: - Pure decisions
    //
    // Factored out and `nonisolated` because they are the only testable part of
    // the failure path, and because each of them has a wrong answer that ships
    // silently: see `AmbientMicOutcome`.

    /// An interruption began means the microphone is gone — an incoming call,
    /// Siri, or another app taking a non-mixable session — and the armed window
    /// has no way to say so except by ending.
    ///
    /// **`.ended` is deliberately `.ignore` rather than a resume.** Resuming would
    /// need two things this feature does not have: a `paused` phase in
    /// `AmbientState` for the orb to show while the tap is dead (there is none, and
    /// an orb still reading `armed` would be lying), and a reactivation of a
    /// session the *system* deactivated for the duration — which the background
    /// cannot do, and the background is where an armed window mostly lives. So an
    /// interruption ends the window and the user re-arms, which is honest, and
    /// fail-safe in the direction that matters: the microphone ends up off.
    ///
    /// Unparseable user info is `.ignore` for a stricter reason than tidiness: the
    /// alternative is guessing, and a guess in the `.fail` direction ends a
    /// perfectly live window on a malformed notification.
    nonisolated static func interruptionOutcome(userInfo: [AnyHashable: Any]?) -> AmbientMicOutcome {
        guard
            let raw = userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
            let type = AVAudioSession.InterruptionType(rawValue: raw)
        else { return .ignore }
        switch type {
        case .began: return .fail(.interrupted)
        case .ended: return .ignore
        @unknown default: return .ignore
        }
    }

    /// A route change is fatal only when it leaves nothing to capture from.
    ///
    /// **The route-change *reason* deliberately takes no part in this decision**,
    /// which is why it is not a parameter. The tempting version switches on it and
    /// treats `.oldDeviceUnavailable` as fatal — and that reason is exactly what
    /// unplugging headphones or walking away from a Bluetooth headset posts, with
    /// the built-in mics still perfectly available behind it. Ambient mode is
    /// specified to follow the user's headset, so those are its *routine* events;
    /// ending the window on them would make every disconnect a disarm. The repair
    /// belongs to `rebuildTap`, which the engine's own configuration-change
    /// notification triggers.
    ///
    /// So the only question worth asking is the one the input inventory answers,
    /// and the answer is independent of how the route came to change.
    nonisolated static func routeChangeOutcome(hasInputRoute: Bool) -> AmbientMicOutcome {
        hasInputRoute ? .ignore : .fail(.inputRouteLost)
    }

    /// Foregrounding is the only moment a revoked microphone permission can be
    /// noticed: revoking it requires a trip to Settings, and there is no
    /// notification for the change itself.
    ///
    /// **`isRunning` is a parameter rather than an early return so the "not
    /// running" answer is the one that gets asserted.** Reporting a failure for a
    /// tap that is not running is not a harmless no-op — `AmbientController` routes
    /// `onFailure` to `disarm`, so it would tear down whatever window exists now,
    /// including a healthy one armed after the tap this notification refers to.
    /// That is the late-`onFailure` hazard `AmbientMicSource.stop()` documents,
    /// reached through the front door.
    ///
    /// In practice iOS usually terminates an app whose microphone permission is
    /// revoked, which takes the window with it and is swept by
    /// `reconcileOnLaunch`. This is the backstop for when it does not.
    nonisolated static func foregroundOutcome(
        isRunning: Bool,
        permissionGranted: Bool
    ) -> AmbientMicOutcome {
        guard isRunning, !permissionGranted else { return .ignore }
        return .fail(.recordPermissionMissing)
    }
}
