import AVFoundation
import XCTest
@testable import Magician

/// What can be proven about the real ambient microphone tap without a microphone.
///
/// **`AVAudioEngine` capture has no input in the simulator**, so `start`, `stop`
/// and the tap callback are not exercised here at all — a test that opened an
/// engine would pass or hang for reasons unrelated to the code. What is covered is
/// everything deliberately factored out of them:
///
/// - `AmbientMicResampler`, which is pure software (`AVAudioConverter` needs no
///   hardware) and carries the two properties the rest of the feature depends on:
///   16 kHz mono out, and an **even byte count** — whole PCM16 samples, never a
///   split one, for whatever consumes the frame (the wake spotter today; the
///   since-unwired `WakePreRoll.append` was the original enforcer of the second).
/// - The notification → outcome mapping, where every wrong answer ships silently:
///   a `.fail` that should have been `.ignore` disarms a window the user was still
///   using, and an `.ignore` that should have been `.fail` leaves the orb claiming
///   to be listening behind a dead tap.
/// - The session category / mode / options triple, which needs no hardware and is
///   the decision most likely to be reverted by someone tidying this file toward
///   its neighbour.
///
/// Three things are deliberately NOT tested, rather than tested badly:
///
/// - **`start`'s permission refusal.** The assertion would read cleanly, but the
///   simulator's record-permission state is whatever the test host was last
///   granted — so on one machine it throws (pass) and on another it proceeds to
///   open an engine against a host microphone. A test whose outcome depends on the
///   developer's privacy settings cannot fail for the right reason.
/// - **`stop()`'s no-callback-in-flight guarantee.** It is a property of a lock
///   held across a real audio-thread delivery; with no tap to deliver from, any
///   test of it would be asserting that a closure this test set to nil is nil.
///   Task 13 owns it — see `docs/components/magios/ambient-mode.md`.
/// - **The generation gate on stale notification hops.** `report` needs a running
///   tap to reach, so with no tap every path returns early for the *wrong* reason
///   and a broken gate would pass identically. Verified by inspection instead: the
///   generation is read before each hop and compared after it.
final class AmbientMicEngineTests: XCTestCase {

    // MARK: - Helpers

    /// A mono float32 buffer of a 440 Hz tone at `rate`, standing in for what the
    /// hardware input node hands the tap. A tone rather than silence on purpose:
    /// silence survives a converter that is dropping every sample.
    private func tone(rate: Double, frames: AVAudioFrameCount) -> AVAudioPCMBuffer {
        let format = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: rate,
            channels: 1,
            interleaved: false
        )!
        // Capacity is clamped to 1 because `AVAudioPCMBuffer` refuses a zero
        // capacity; `frameLength` is what the empty-buffer case actually needs.
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: max(1, frames))!
        buffer.frameLength = frames
        let channel = buffer.floatChannelData![0]
        // Every term annotated: written as one inline expression this defeats the
        // type checker outright ("unable to type-check in reasonable time").
        let radiansPerSample: Double = 2.0 * Double.pi * 440.0 / rate
        for i in 0 ..< Int(frames) {
            let sample: Double = 0.5 * sin(radiansPerSample * Double(i))
            channel[i] = Float(sample)
        }
        return buffer
    }

    private func int16s(_ data: Data) -> [Int16] {
        stride(from: 0, to: data.count - 1, by: 2).map { i in
            Int16(bitPattern: UInt16(data[data.startIndex + i]) | (UInt16(data[data.startIndex + i + 1]) << 8))
        }
    }

    // MARK: - Resampler

    func testTargetFormatIsWhatTheSpotterIsSpecifiedIn() {
        let format = AmbientMicResampler.targetFormat
        XCTAssertEqual(format.sampleRate, 16_000)
        XCTAssertEqual(format.channelCount, 1)
        XCTAssertEqual(format.commonFormat, .pcmFormatFloat32)
    }

    /// 0.1 s of 48 kHz audio must come out as roughly 0.1 s of 16 kHz PCM16, with
    /// signal in it. The range is wide because a sample-rate converter primes on
    /// its first call and returns slightly short; the point is the ratio, not the
    /// exact frame count.
    func testEncodeResamplesTo16kHzPCM16() throws {
        let resampler = AmbientMicResampler()
        let encoded = try XCTUnwrap(resampler.encode(tone(rate: 48_000, frames: 4800)))
        let samples = int16s(encoded)
        XCTAssertGreaterThan(samples.count, 1_200, "0.1 s at 16 kHz is 1600 samples; a third of that is a wrong rate")
        XCTAssertLessThanOrEqual(samples.count, 1_700)
        XCTAssertTrue(samples.contains { abs($0) > 1_000 }, "a tone at half scale must survive as signal, not silence")
    }

    /// The whole-PCM16-sample contract: an odd byte count splits a sample and
    /// byte-swaps every decoded value after it into plausible noise. Odd input
    /// frame counts are the interesting case: they are what a real tap delivers
    /// after a route change, and a length in *samples* accidentally used as a
    /// length in *bytes* shows up here and nowhere else.
    func testEncodedByteCountIsAlwaysEven() throws {
        let resampler = AmbientMicResampler()
        var checked = 0
        for frames in [1, 3, 17, 511, 4_095] as [AVAudioFrameCount] {
            guard let encoded = resampler.encode(tone(rate: 44_100, frames: frames)) else { continue }
            checked += 1
            XCTAssertTrue(
                encoded.count.isMultiple(of: 2),
                "\(frames) input frames produced \(encoded.count) bytes; a PCM16 reader would byte-swap everything after the split sample"
            )
        }
        // Without this the test passes vacuously. A converter legitimately returns
        // nil for the first few frames while it primes, so `if let` alone can skip
        // every iteration and go green having asserted nothing — standing exactly
        // where the whole-sample contract is supposed to be pinned.
        XCTAssertGreaterThanOrEqual(
            checked, 2,
            "the loop produced no bytes to check (\(checked)); this assertion never ran"
        )
    }

    /// The resampler carries no assumption about a format it was never told to
    /// expect: hand it a buffer at a different sample rate and it re-derives rather
    /// than resampling from a rate that is no longer arriving (which yields silence
    /// or nothing at all).
    ///
    /// **This does not pin the route-change path**, and it would overstate the test
    /// to say so. One resampler belongs to one tap, and `installTap`'s `format:`
    /// fixes that tap's buffer format, so in production this branch is unreachable.
    /// What survives a headset being connected or disconnected mid-window is
    /// `rebuildTap` — new engine configuration, new tap, new resampler — and that
    /// needs hardware, so it is Task 13's (device item 3). This is the defensive
    /// half, kept because it is free and because the assumption it rules out is
    /// exactly the one `ListenController` makes.
    func testEncodeFollowsAnInputFormatChange() throws {
        let resampler = AmbientMicResampler()
        let first = try XCTUnwrap(resampler.encode(tone(rate: 48_000, frames: 4_800)))
        let second = try XCTUnwrap(resampler.encode(tone(rate: 16_000, frames: 1_600)))
        XCTAssertTrue(int16s(first).contains { abs($0) > 1_000 })
        XCTAssertTrue(
            int16s(second).contains { abs($0) > 1_000 },
            "a buffer at a new sample rate produced no signal — the converter was not rebuilt"
        )
    }

    func testEncodeIgnoresAnEmptyBuffer() {
        let resampler = AmbientMicResampler()
        XCTAssertNil(resampler.encode(tone(rate: 48_000, frames: 0)))
    }

    /// A capacity of zero makes `AVAudioPCMBuffer` return nil, which would drop
    /// every short buffer on the floor. The naive `frames * ratio` does exactly
    /// that when downsampling anything under three frames.
    func testCapacityHasHeadroomAndIsNeverZero() {
        XCTAssertGreaterThan(
            AmbientMicResampler.capacity(inputFrames: 1, inputRate: 48_000, targetRate: 16_000),
            0
        )
        // 4096 frames at 48 kHz is 1365.33 at 16 kHz — the capacity must exceed
        // the truncated ratio, not equal it.
        XCTAssertGreaterThan(
            AmbientMicResampler.capacity(inputFrames: 4_096, inputRate: 48_000, targetRate: 16_000),
            1_365
        )
        // Upsampling must not under-allocate either.
        XCTAssertGreaterThanOrEqual(
            AmbientMicResampler.capacity(inputFrames: 1_024, inputRate: 8_000, targetRate: 16_000),
            2_048
        )
        // A degenerate format cannot be allowed to produce a zero capacity.
        XCTAssertGreaterThan(
            AmbientMicResampler.capacity(inputFrames: 512, inputRate: 0, targetRate: 16_000),
            0
        )
    }

    // MARK: - Session configuration

    /// The triple needs no hardware to assert and is the decision here most likely
    /// to be quietly reverted toward `ListenController`'s, which omits
    /// `.allowBluetoothHFP` so a phone on a table hears the room. Ambient mode has
    /// the inverse requirement — capture follows the headset the user is wearing —
    /// and the *omission* is the mechanism, so dropping the option would look like
    /// tidying rather than like a behaviour change.
    func testSessionConfigurationFollowsTheHeadsetAndDefaultsToTheSpeaker() {
        XCTAssertEqual(AmbientMicEngine.sessionCategory, .playAndRecord)
        XCTAssertEqual(
            AmbientMicEngine.sessionMode,
            .voiceChat,
            "the armed window configures the session ONCE, in the foreground, with the mode the conversation needs"
        )
        XCTAssertTrue(
            AmbientMicEngine.sessionOptions.contains(.defaultToSpeaker),
            "the spoken reply has to be audible with the phone face-up on a table"
        )
        XCTAssertTrue(
            AmbientMicEngine.sessionOptions.contains(.allowBluetoothHFP),
            "without this a connected headset's microphone is not offered as an input at all"
        )
        XCTAssertFalse(
            AmbientMicEngine.sessionOptions.contains(.mixWithOthers),
            "unresolved whether a mixable session can be activated from the background — design §15"
        )
    }

    /// **The fix for this feature's headline device failure, stated as one equality.**
    ///
    /// Arming worked and the wake phrase fired while the app was backgrounded — which
    /// proves the session was active and the spotting tap ran off screen — and then
    /// the microphone was lost. The cause was the handoff: the armed window had
    /// configured the session as `.spokenAudio` and `VoiceAudioEngine.start`
    /// **changed the mode**, re-stated a preferred sample rate and called
    /// `setActive(true)`, from the background, before rebuilding the audio unit with
    /// voice processing. The input node came back degenerate, `installTap`'s guard
    /// refused it, and `resumeSpotting` disarmed with "Lost the microphone."
    ///
    /// Two halves fix it and this test pins the first: the window configures the
    /// session with the mode the *conversation* needs, so there is nothing for the
    /// handoff to change. The second half — that the conversation does not touch the
    /// session even so — is `VoiceCallViewModelTests`'
    /// `testAnAmbientStartDoesNotConfigureTheSharedSession`. **Either half alone is
    /// insufficient**: matching triples with a live `setActive(true)` still
    /// re-activates from the background, and a skipped configuration over mismatched
    /// triples means the conversation runs without AEC and answers its own replies.
    ///
    /// Asserted as an equality rather than as two literals so that editing either
    /// side fails here. `AVAudioEngine` capture has no input in the simulator, so this
    /// is the only place the property is checkable at all.
    func testTheArmedWindowConfiguresTheSessionTheConversationNeeds() {
        XCTAssertEqual(
            AmbientMicEngine.sessionCategory,
            VoiceAudioEngine.callSessionCategory,
            "a category change mid-handoff is a re-configuration of a session the window owns"
        )
        XCTAssertEqual(
            AmbientMicEngine.sessionMode,
            VoiceAudioEngine.callSessionMode,
            "a MODE change is what broke it: the handoff must require no session change at all"
        )
        XCTAssertEqual(
            AmbientMicEngine.sessionOptions,
            VoiceAudioEngine.callSessionOptions,
            "the options are part of the same triple; a mismatch is the same re-configuration"
        )
    }

    // MARK: - Interruption mapping

    private func interruption(_ type: AVAudioSession.InterruptionType) -> [AnyHashable: Any] {
        [AVAudioSessionInterruptionTypeKey: type.rawValue]
    }

    /// An incoming call or Siri takes the microphone, and the window has no
    /// `paused` phase to show — so it ends rather than letting the orb claim the
    /// user is still being heard.
    func testInterruptionBeganEndsTheWindow() {
        XCTAssertEqual(
            AmbientMicEngine.interruptionOutcome(userInfo: interruption(.began)),
            .fail(.interrupted)
        )
    }

    /// The counterpart, and the one an over-eager reading gets wrong: the
    /// interruption ENDING must not itself be a second failure. `began` has already
    /// taken the window down, and `report` is single-shot, but a mapping that
    /// failed here would end the *next* window on the first interruption's tail.
    func testInterruptionEndedIsNotAFailure() {
        XCTAssertEqual(
            AmbientMicEngine.interruptionOutcome(userInfo: interruption(.ended)),
            .ignore
        )
    }

    /// Guessing in the `.fail` direction on a malformed notification would end a
    /// live window for no reason at all.
    func testMalformedInterruptionIsIgnored() {
        XCTAssertEqual(AmbientMicEngine.interruptionOutcome(userInfo: nil), .ignore)
        XCTAssertEqual(AmbientMicEngine.interruptionOutcome(userInfo: [:]), .ignore)
        XCTAssertEqual(
            AmbientMicEngine.interruptionOutcome(userInfo: [AVAudioSessionInterruptionTypeKey: "began"]),
            .ignore
        )
        XCTAssertEqual(
            AmbientMicEngine.interruptionOutcome(userInfo: [AVAudioSessionInterruptionTypeKey: UInt(99)]),
            .ignore
        )
    }

    // MARK: - Route change mapping

    /// The headset case, which is ambient mode's *routine* event rather than its
    /// failure: capture follows whatever the user is wearing, so connecting and
    /// disconnecting one happens mid-window by design. An input route remains, so
    /// the window survives and `rebuildTap` re-tunes it.
    func testARouteChangeThatLeavesAnInputIsNotAFailure() {
        XCTAssertEqual(AmbientMicEngine.routeChangeOutcome(hasInputRoute: true), .ignore)
    }

    func testARouteChangeThatLeavesNoInputEndsTheWindow() {
        XCTAssertEqual(
            AmbientMicEngine.routeChangeOutcome(hasInputRoute: false),
            .fail(.inputRouteLost)
        )
    }

    // MARK: - Foreground permission re-check

    func testRevokedPermissionEndsARunningWindow() {
        XCTAssertEqual(
            AmbientMicEngine.foregroundOutcome(isRunning: true, permissionGranted: false),
            .fail(.recordPermissionMissing)
        )
    }

    /// The assertion the parameter exists for. A foreground with no tap running is
    /// the overwhelmingly common case — every launch, every app switch — and
    /// reporting a failure there is not a harmless no-op: `AmbientController` routes
    /// `onFailure` to `disarm`, so it would tear down whichever window exists now,
    /// including one armed after the tap the notification refers to.
    func testAForegroundWithNoTapRunningReportsNothing() {
        XCTAssertEqual(
            AmbientMicEngine.foregroundOutcome(isRunning: false, permissionGranted: false),
            .ignore
        )
        XCTAssertEqual(
            AmbientMicEngine.foregroundOutcome(isRunning: false, permissionGranted: true),
            .ignore
        )
    }

    func testAForegroundWithPermissionIntactReportsNothing() {
        XCTAssertEqual(
            AmbientMicEngine.foregroundOutcome(isRunning: true, permissionGranted: true),
            .ignore
        )
    }

    // MARK: - Lifecycle safety

    /// `AmbientController.disarm` calls `mic.stop()` unconditionally, including on
    /// windows whose tap was never opened and on ones the wake handoff already
    /// released — so "nothing to stop" is the common path, not an edge case. An
    /// implementation reaching through `engine!` crashes an ordinary disarm.
    ///
    /// No hardware is touched: the engine is constructed lazily inside `start`.
    @MainActor
    func testStopWithNothingRunningIsSafeAndRepeatable() {
        let engine = AmbientMicEngine()
        engine.stop()
        engine.stop()
    }
}
