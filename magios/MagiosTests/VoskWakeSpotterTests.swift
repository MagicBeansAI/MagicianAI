@preconcurrency import AVFoundation
import XCTest

@testable import Magician

/// Renders speech to 16 kHz mono PCM16 — the exact format `WakeSpotter.feed`
/// takes — with `AVSpeechSynthesizer`'s offline write path.
///
/// **Why synthesised rather than committed WAVs.** The alternative is ~20
/// two-second clips at 16 kHz, about 1.3 MB of binary fixtures, and they would be
/// produced by this same synthesiser and then frozen. Freezing buys determinism
/// and costs the ability to add a near-miss phrase without generating and
/// committing another blob — and the near-miss list is the part of this suite
/// most likely to grow, because it is the part that found the product problem.
/// The cost is real and is stated plainly: the *absolute* numbers below are a
/// property of this OS version's voices, so a major-version bump can move them.
/// That is why the near-miss case is written as a measurement with a ceiling
/// rather than as an equality, and why it prints the whole table.
///
/// The renderer is offline (no audio session, no hardware), which is what keeps
/// the whole suite runnable in the simulator.
@MainActor
enum SpokenAudio {

    private static var cache: [String: Data] = [:]
    private static let synthesizer = AVSpeechSynthesizer()
    private static let target = AVAudioFormat(
        commonFormat: .pcmFormatInt16, sampleRate: 16_000, channels: 1, interleaved: true
    )!

    // MARK: - synthetic speakers

    /// One rendering configuration: a specific installed voice at a specific
    /// speaking rate and pitch.
    ///
    /// **This is not a speaker.** It is a TTS voice with two knobs moved, which
    /// varies timbre and timing but shares one synthesiser's pronunciation model
    /// — see `VoskWakeAccuracyMeasurementTests.trueAccepts(_:speakers:)` for
    /// exactly what that does and does not approximate.
    struct Speaker: Hashable {
        let voiceIdentifier: String
        let name: String
        let language: String
        /// `nil` leaves `AVSpeechUtterance`'s default in place rather than
        /// re-supplying it. That is what keeps the default prosody cell on the
        /// same code path as every near-miss clip in this file — and for
        /// Samantha, the voice `pcm16(_:)` picks for `en-US`, it renders exactly
        /// what the single-voice columns were measured with (`hey presto`: 808 ms
        /// in both).
        let rate: Float?
        let pitch: Float?

        var label: String {
            var prosody: [String] = []
            if let rate { prosody.append("rate " + String(format: "%.2f", rate)) }
            if let pitch { prosody.append("pitch " + String(format: "%.2f", pitch)) }
            let suffix = prosody.isEmpty ? "default" : prosody.joined(separator: " ")
            return name + " (" + language + ", " + suffix + ")"
        }
    }

    /// English locales the matrix will accept a voice from. Whatever the
    /// simulator actually has is a subset of this and is **enumerated at run
    /// time and printed** — a rate over a thin voice set is only honest if the
    /// set is named next to it.
    static let englishLocales = ["en-US", "en-GB", "en-AU", "en-IN", "en-IE", "en-ZA"]

    /// Every installed voice in `englishLocales`, in a stable order — **novelty
    /// voices included**, which is why almost nothing should call this.
    static func allEnglishVoices() -> [AVSpeechSynthesisVoice] {
        AVSpeechSynthesisVoice.speechVoices()
            .filter { englishLocales.contains($0.language) }
            .sorted { ($0.language, $0.name, $0.identifier) < ($1.language, $1.name, $1.identifier) }
    }

    /// The subset that is trying to sound like a person.
    ///
    /// **The simulator's en-US list is mostly novelty voices** — Bells and Jester
    /// sing the text, Zarvox and Trinoids are robots, Bad News and Organ are
    /// sound effects with words in them. Feeding those to a wake spotter measures
    /// whether Vosk can transcribe a singing robot, and a "true-accept rate"
    /// dragged down by them would read as a fact about the phrase. They are
    /// excluded by Apple's own trait rather than by a name list, so the exclusion
    /// cannot drift as the image changes.
    ///
    /// Personal voices are excluded too: none exists in CI, and one that did
    /// would make the denominator machine-specific in a way nobody could reproduce.
    static func englishVoices() -> [AVSpeechSynthesisVoice] {
        allEnglishVoices().filter { !isExcluded($0) }
    }

    static func isExcluded(_ voice: AVSpeechSynthesisVoice) -> Bool {
        voice.voiceTraits.contains(.isNoveltyVoice) || voice.voiceTraits.contains(.isPersonalVoice)
    }

    /// 16 kHz mono PCM16 for `text` as rendered by `speaker`.
    ///
    /// **Deliberately NOT cached, unlike `pcm16(_:)`.** The near-miss sets are
    /// re-read many times per run, so caching them is most of what makes the
    /// suite fast. A speaker rendering is the opposite: the matrix asks for each
    /// (phrase, speaker) pair exactly once, so a cache would retain ~950 clips to
    /// serve the ~50 the inventory case re-renders — tens of megabytes held for
    /// the life of the test class, in a process that also holds a 68 MB speech
    /// model. That retention crashed the full suite once; see
    /// `VoskWakeAccuracyMeasurementTests.trueAccepts(_:speakers:)`.
    static func pcm16(_ text: String, as speaker: Speaker) async -> Data {
        await render(
            text, voice: AVSpeechSynthesisVoice(identifier: speaker.voiceIdentifier),
            rate: speaker.rate, pitch: speaker.pitch
        )
    }

    /// What `write` actually delivered — kept so a suite that renders nothing can
    /// say WHY rather than just failing every audio assertion identically.
    struct Diagnostics {
        var bufferCount = 0
        var totalFrames: AVAudioFrameCount = 0
        var sourceFormat = "none"
        var callbackOnMainThread: Bool?
        var timedOut = false
        var convertedBytes = 0
    }

    private(set) static var lastDiagnostics = Diagnostics()

    /// 16 kHz mono PCM16 for `text`. Empty means the platform declined to render.
    ///
    /// `async` and NON-BLOCKING is load-bearing: `AVSpeechSynthesizer` delivers
    /// its buffers on the main queue, so the obvious semaphore version deadlocks
    /// against a `@MainActor` test and every render times out empty.
    static func pcm16(_ text: String) async -> Data {
        if let hit = cache[text] { return hit }
        let rendered = await render(text)
        // Deliberately NOT cached when empty: caching a failure poisons every
        // later case with a fast, quiet zero instead of one loud one.
        if !rendered.isEmpty { cache[text] = rendered }
        return rendered
    }

    private final class Collector {
        var chunks: [AVAudioPCMBuffer] = []
        var finished = false
        var onMainThread: Bool?
    }

    private static func render(
        _ text: String,
        voice: AVSpeechSynthesisVoice? = AVSpeechSynthesisVoice(language: "en-US"),
        rate: Float? = nil,
        pitch: Float? = nil
    ) async -> Data {
        let utterance = AVSpeechUtterance(string: text)
        utterance.voice = voice
        // Assigned only when asked for: re-supplying `AVSpeechUtterance`'s own
        // defaults would make the default cell a different code path from every
        // clip the near-miss columns were measured with.
        if let rate { utterance.rate = rate }
        if let pitch { utterance.pitchMultiplier = pitch }
        let collector = Collector()
        var diagnostics = Diagnostics()

        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            let timeout = DispatchWorkItem {
                guard !collector.finished else { return }
                collector.finished = true
                diagnostics.timedOut = true
                continuation.resume()
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 20, execute: timeout)
            synthesizer.write(utterance) { buffer in
                if collector.onMainThread == nil { collector.onMainThread = Thread.isMainThread }
                guard let pcm = buffer as? AVAudioPCMBuffer else { return }
                guard pcm.frameLength > 0 else {  // terminator
                    guard !collector.finished else { return }
                    collector.finished = true
                    timeout.cancel()
                    continuation.resume()
                    return
                }
                // The synthesiser reuses its buffer, so copy before returning.
                guard let copy = AVAudioPCMBuffer(pcmFormat: pcm.format, frameCapacity: pcm.frameLength)
                else { return }
                copy.frameLength = pcm.frameLength
                let bytes = Int(pcm.frameLength) * Int(pcm.format.streamDescription.pointee.mBytesPerFrame)
                memcpy(copy.audioBufferList.pointee.mBuffers.mData,
                       pcm.audioBufferList.pointee.mBuffers.mData, bytes)
                collector.chunks.append(copy)
            }
        }

        diagnostics.bufferCount = collector.chunks.count
        diagnostics.totalFrames = collector.chunks.reduce(0) { $0 + $1.frameLength }
        diagnostics.callbackOnMainThread = collector.onMainThread
        if let format = collector.chunks.first?.format { diagnostics.sourceFormat = "\(format)" }
        let data = concatenate(collector.chunks).map(convertToTarget) ?? Data()
        diagnostics.convertedBytes = data.count
        lastDiagnostics = diagnostics
        return data
    }

    private static func concatenate(_ buffers: [AVAudioPCMBuffer]) -> AVAudioPCMBuffer? {
        guard let format = buffers.first?.format else { return nil }
        let frames = buffers.reduce(0) { $0 + $1.frameLength }
        guard frames > 0, let out = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames)
        else { return nil }
        let bytesPerFrame = Int(format.streamDescription.pointee.mBytesPerFrame)
        var offset = 0
        for buffer in buffers {
            let bytes = Int(buffer.frameLength) * bytesPerFrame
            memcpy(out.audioBufferList.pointee.mBuffers.mData!.advanced(by: offset),
                   buffer.audioBufferList.pointee.mBuffers.mData, bytes)
            offset += bytes
        }
        out.frameLength = frames
        return out
    }

    /// Resample once over the whole utterance rather than per chunk — converting
    /// each buffer separately leaves a discontinuity at every boundary, which is
    /// audible to a decoder even when it is not to a person.
    private static func convertToTarget(_ source: AVAudioPCMBuffer) -> Data {
        if source.format == target, let channel = source.int16ChannelData {
            return Data(bytes: channel[0], count: Int(source.frameLength) * MemoryLayout<Int16>.size)
        }
        guard let converter = AVAudioConverter(from: source.format, to: target) else { return Data() }
        let ratio = target.sampleRate / source.format.sampleRate
        let capacity = AVAudioFrameCount(Double(source.frameLength) * ratio) + 4096
        guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return Data() }
        var supplied = false
        var error: NSError?
        converter.convert(to: out, error: &error) { _, status in
            if supplied { status.pointee = .endOfStream; return nil }
            supplied = true
            status.pointee = .haveData
            return source
        }
        guard error == nil, out.frameLength > 0, let channel = out.int16ChannelData else { return Data() }
        return Data(bytes: channel[0], count: Int(out.frameLength) * MemoryLayout<Int16>.size)
    }
}

@MainActor
final class VoskWakeSpotterTests: XCTestCase {

    /// The phrase every case is armed with. In the model's lexicon — the suite
    /// asserts that separately, because if it ever stops being true the rest of
    /// these results become meaningless rather than merely wrong.
    private static let phrase = "hey magician"

    /// Phonetically adjacent to the phrase, and none of them a wake. This is the
    /// list that carries the product finding; see `testNearMissFalseAcceptRate`.
    private static let nearMisses = [
        "hey musician", "hey machine", "hey position", "hey magic", "hey magazine",
        "hey mission", "hey physician", "hey magnesium", "a magician", "the magician",
        "hey imagine", "hey mechanic", "hey message", "hey managing", "hey medicine",
    ]

    /// Ordinary speech that shares nothing with the phrase. These exercise the
    /// `[unk]` guarantee and are held to ZERO.
    private static let unrelated = [
        "what time is the meeting tomorrow",
        "can you pass me the salt please",
        "the weather forecast looks good for the weekend",
        "i need to pick up groceries on the way home",
        "let me know when you are free to talk",
        "the train was delayed by twenty minutes again",
        "she said the report would be ready by friday",
        "there is a new coffee place around the corner",
    ]

    /// One spotter for the whole suite: constructing one is cheap, but the first
    /// `configure` loads a 68 MB model and that cost is worth paying once.
    /// `configure` fully replaces phrase state, which is what makes sharing safe
    /// — and is itself asserted by `testConfigureReplacesRatherThanAppends`.
    private static var sharedSpotter: VoskWakeSpotter?

    private var spotter: VoskWakeSpotter!
    private var hits: [WakeHit] = []
    private var hitWasOnMainThread: [Bool] = []

    override func setUp() async throws {
        try await super.setUp()
        try XCTSkipIf(VoskWakeSpotter.bundledModelURL == nil,
                      "No bundled Vosk model — run `make setup-magios-vosk` and regenerate the project.")
        if Self.sharedSpotter == nil { Self.sharedSpotter = VoskWakeSpotter() }
        spotter = Self.sharedSpotter
        hits = []
        hitWasOnMainThread = []
        spotter.onHit = { [weak self] hit in
            self?.hits.append(hit)
            self?.hitWasOnMainThread.append(Thread.isMainThread)
        }
    }

    override func tearDown() async throws {
        spotter?.onHit = nil
        spotter = nil
        try await super.tearDown()
    }

    // MARK: - helpers

    /// Trailing silence fed after every utterance.
    ///
    /// **This is 1.5 s rather than a token 200 ms because the spotter fires on
    /// FINALISED results**, and a finalised result only exists once the decoder
    /// has decided the utterance ended — which takes silence. With a short tail
    /// nothing ever finalises and every audio case silently reports "no wake",
    /// which is the same class of harness failure as the synthesiser deadlock:
    /// a suite that measures nothing and reads as a pass.
    ///
    /// It is also the honest model of a real armed window, which runs for minutes
    /// and where silence always arrives eventually.
    private static let trailingSilenceMs = 1_500

    private func silence(_ milliseconds: Int) -> Data {
        Data(count: milliseconds * 32)  // 16 kHz mono PCM16 = 32 bytes/ms
    }

    /// Feed an utterance the way the microphone would: 100 ms frames, then enough
    /// trailing silence for the decoder to finalise it.
    private func speak(_ text: String) async {
        let pcm = await SpokenAudio.pcm16(text)
        XCTAssertFalse(pcm.isEmpty, "AVSpeechSynthesizer rendered nothing for \"\(text)\"")
        feed(pcm)
        feed(silence(Self.trailingSilenceMs))
        await settle()
    }

    private func feed(_ pcm: Data) {
        let frame = 1_600 * MemoryLayout<Int16>.size  // 100 ms at 16 kHz
        var offset = 0
        while offset < pcm.count {
            let end = min(offset + frame, pcm.count)
            spotter.feed(pcm.subdata(in: offset..<end))
            offset = end
        }
    }

    /// Let the `Task { @MainActor }` the spotter enqueues actually run.
    private func settle() async {
        for _ in 0..<3 { await Task.yield() }
        try? await Task.sleep(nanoseconds: 50_000_000)
    }

    /// Arm, speak, and report whether it woke — resetting first so no case can
    /// inherit another's decoder state or fire cooldown.
    private func wakes(on text: String) async -> Bool {
        spotter.reset()
        hits = []
        await speak(text)
        return !hits.isEmpty
    }

    // MARK: - renderer probe

    /// Reports what `AVSpeechSynthesizer.write` actually produced. Not a product
    /// assertion — it exists so "every audio case failed" can be diagnosed in one
    /// place instead of fifteen.
    func testSynthesiserProducesAudio() async throws {
        let pcm = await SpokenAudio.pcm16(Self.phrase)
        let d = SpokenAudio.lastDiagnostics
        print("""

        ┌── TTS RENDERER PROBE ────────────────────────────────────────────────
        │ text            : "\(Self.phrase)"
        │ buffers          : \(d.bufferCount)
        │ total frames     : \(d.totalFrames)
        │ source format    : \(d.sourceFormat)
        │ callback on main : \(d.callbackOnMainThread.map(String.init(describing:)) ?? "never called")
        │ timed out        : \(d.timedOut)
        │ converted bytes  : \(d.convertedBytes)  (= \(d.convertedBytes / 32) ms @ 16 kHz PCM16)
        └──────────────────────────────────────────────────────────────────────

        """)
        XCTAssertFalse(pcm.isEmpty, "AVSpeechSynthesizer produced no audio — see the probe table above")
    }

    // MARK: - the phrase itself

    func testExactPhraseWakes() async throws {
        spotter.configure(phrases: [Self.phrase])
        XCTAssertTrue(spotter.isArmed)
        XCTAssertEqual(spotter.rejectedPhrases, [])
        let woke = await wakes(on: Self.phrase)
        XCTAssertTrue(woke, "the configured phrase did not wake the spotter")
        XCTAssertEqual(hits.first?.phrase, Self.phrase, "the hit must report the phrase as configured")
    }

    /// The `[unk]` guarantee, and the one number in this file that is held to
    /// zero. A false accept here opens a microphone to the network, which is the
    /// single thing the local gate exists to prevent.
    func testUnrelatedSpeechNeverWakes() async throws {
        spotter.configure(phrases: [Self.phrase])
        var accepted: [String] = []
        for line in Self.unrelated {
            if await wakes(on: line) { accepted.append(line) }
        }
        XCTAssertEqual(accepted, [], "unrelated speech woke the spotter — check that [unk] is in the grammar")
    }

    /// **The product finding.** Vosk is bad at phonetically adjacent phrases, and
    /// this measures how bad rather than asserting it away.
    ///
    /// There is no confidence signal to filter on: near-miss accepts come back at
    /// confidence 1.0, indistinguishable from a real wake. The ceiling below is a
    /// REGRESSION GUARD pinned to the measured rate, not a target anyone designed
    /// for — the mitigations (requiring a "hey" prefix, preferring longer
    /// phrases, a second-trigger confirmation) are product decisions and none of
    /// them is implemented here.
    func testNearMissFalseAcceptRate() async throws {
        spotter.configure(phrases: [Self.phrase])
        var accepted: [String] = []
        var rejected: [String] = []
        for candidate in Self.nearMisses {
            if await wakes(on: candidate) { accepted.append(candidate) } else { rejected.append(candidate) }
        }
        let rate = Double(accepted.count) / Double(Self.nearMisses.count)
        print("""

        ┌── NEAR-MISS FALSE ACCEPTS ───────────────────────────────────────────
        │ phrase armed : "\(Self.phrase)"
        │ false accepts: \(accepted.count) / \(Self.nearMisses.count) \
        (\(String(format: "%.0f%%", rate * 100)))
        │ ACCEPTED (woke the spotter, should not have):
        \(accepted.map { "│   ✗ \($0)" }.joined(separator: "\n"))
        │ REJECTED (correct):
        \(rejected.map { "│   ✓ \($0)" }.joined(separator: "\n"))
        └──────────────────────────────────────────────────────────────────────

        """)
        // MEASURED BASELINE: 2 of 15 (13%) through the shipping type on iOS 26
        // simulator voices, with finalised results. The ceiling is 3 — the
        // measured value plus one, so ordinary synthesiser variation is not a red
        // build while a real regression still is.
        //
        // **This is a REGRESSION GUARD ON A MEASURED VALUE, not a target.** Nobody
        // designed for 13%; it is what the engine does with a grammar, `[unk]` and
        // finals, and it is recorded here so a change that makes it worse fails
        // loudly. In particular, reverting the hit path to partials takes this to
        // 11/15 and this assertion is what would catch it.
        //
        // The remaining two mitigations — a mandatory prefix and second-trigger
        // confirmation — are product decisions and are deliberately not taken.
        XCTAssertLessThanOrEqual(
            accepted.count, 3,
            """
            Near-miss false accepts regressed past the measured baseline of 2/15. \
            If the hit path went back to reading partials, that is the bug (11/15). \
            If it moved because the OS voices changed, re-measure and record the new \
            number in docs/components/magios/ambient-mode.md.
            """
        )
    }

    // MARK: - lexicon validation

    /// Out-of-lexicon words are dropped from a Vosk grammar with only a log line,
    /// so "hey magican" would arm as the bare phrase "hey" and fire on any sentence
    /// containing it. Rejecting the phrase whole is the only safe reading.
    func testOutOfLexiconPhraseIsRejectedRatherThanSilentlyShortened() async throws {
        spotter.configure(phrases: ["hey magican"])
        XCTAssertEqual(spotter.rejectedPhrases, ["hey magican"])
        XCTAssertFalse(spotter.isArmed, "a phrase with an unknown word must not arm a shortened grammar")
        // The proof that it did not silently become "hey": say a sentence with
        // "hey" in it and require silence.
        let woke = await wakes(on: "hey there how are you doing today")
        XCTAssertFalse(woke, "the rejected phrase armed as a shortened prefix")
    }

    func testUsablePhraseSurvivesAlongsideARejectedOne() throws {
        spotter.configure(phrases: [Self.phrase, "hey magican"])
        XCTAssertEqual(spotter.rejectedPhrases, ["hey magican"])
        XCTAssertTrue(spotter.isArmed, "one bad phrase must not disarm the good ones")
    }

    // MARK: - the two grounds a phrase can be unusable

    /// In-lexicon and armed, and its measured rate reported alongside. The two
    /// grounds are separate because they need separate answers: one cannot arm at
    /// all, the other arms and merely has a number attached to it.
    func testAMeasuredPhraseArmsAndIsReportedSeparatelyFromALexiconRejection() throws {
        spotter.configure(phrases: ["sam"])
        XCTAssertTrue(spotter.isArmed, "the worst phrase in the matrix still arms — the rate is reported, not enforced")
        XCTAssertEqual(spotter.rejectedPhrases, [], "\"sam\" IS in the lexicon; it must not be reported as missing")
        XCTAssertEqual(spotter.phraseNotes.map(\.phrase), ["sam"])
        XCTAssertEqual(
            VoskWakeSpotter.assessment(of: "sam"),
            .measured(nearMissFalseAcceptPercent: 80, syntheticTrueAcceptPercent: 100, note: spotter.phraseNotes[0].note)
        )
    }

    /// The distinction Task 8 has to act on, in one configure.
    ///
    /// **The armed phrases BOTH carry a note, and that is the change.** A rate of
    /// 20% and a rate of 80% are two different facts, and the previous shape put
    /// only one of them anywhere — the good phrase was reported by being absent,
    /// which is also how an unmeasured phrase was reported.
    func testLexiconRejectionAndMeasurementAreReportedIndependently() throws {
        spotter.configure(phrases: ["hey magican", "sam", "hey magician"])
        XCTAssertEqual(spotter.rejectedPhrases, ["hey magican"])
        XCTAssertTrue(spotter.isArmed)
        XCTAssertEqual(spotter.phraseNotes.map(\.phrase), ["sam", "hey magician"])
        // The rejected phrase is in exactly one bucket, and it is not this one:
        // it never armed, so there is nothing measured about it to show.
        XCTAssertFalse(spotter.phraseNotes.contains { $0.phrase == "hey magican" })
    }

    // MARK: - what the classification claims, and what it refuses to

    /// **The rule this suite used to pin, and the run that falsified it.**
    ///
    /// The retired rule flagged a phrase whose longest word was 3 letters or
    /// fewer. `hey sammy` (5) is 46% and passed it; `hey sam` (3) is 53% and
    /// failed it — one utterance apart at n=15. And the direction reverses:
    /// `hey samy` (4 letters) is 6%, the best row ever measured, and a `<= 5` cut
    /// would have flagged it while a `<= 4` cut flags it alone.
    func testLetterCountDoesNotOrderTheMeasuredRates() {
        let byLongestWord = ["hey samy": 4, "hey magician": 8, "hey presto": 6,
                             "hey sammy": 5, "hey sammie": 6, "hey sam": 3]
        for (phrase, longest) in byLongestWord {
            let rate = VoskWakeSpotter.measuredNearMissFalseAcceptPercent[phrase]
            XCTAssertNotNil(rate, "\(phrase) must be in the measured table")
            XCTAssertEqual(
                phrase.split(separator: " ").map(\.count).max(), longest,
                "the letter count this case reasons about drifted from the phrase"
            )
        }
        // Monotonicity is the property the retired rule assumed. Sorted by rate,
        // the letter counts are 4, 8, 6, 5, 6, 3 — not sorted, in either
        // direction, and not one swap away from it either.
        let byRate = byLongestWord.keys.sorted {
            VoskWakeSpotter.measuredNearMissFalseAcceptPercent[$0]! < VoskWakeSpotter.measuredNearMissFalseAcceptPercent[$1]!
        }
        let lengths = byRate.map { byLongestWord[$0]! }
        XCTAssertNotEqual(lengths, lengths.sorted(), "letter count would have to be non-monotone for the rule to be wrong")
        XCTAssertNotEqual(lengths, lengths.sorted(by: >))
        // And no cut separates them: whatever threshold t is chosen, the flagged
        // set is never the badly-measured set.
        for threshold in 3...8 {
            let flagged = Set(byLongestWord.filter { $0.value <= threshold }.keys)
            let worst = Set(["hey sam", "hey sammy", "hey sammie"])
            XCTAssertNotEqual(flagged, worst, "a <= \(threshold) letter cut would have separated the rows")
        }
    }

    /// **Every phrase the matrix has a number for carries that number**, and the
    /// number is the one in the table rather than a verdict derived from it.
    func testAMeasuredPhraseCarriesItsRate() {
        XCTAssertEqual(
            VoskWakeSpotter.assessment(of: "Hey Presto!"),
            .measured(nearMissFalseAcceptPercent: 33, syntheticTrueAcceptPercent: 88, note: VoskWakeSpotter.assessment(of: "hey presto").note!)
        )
        guard case .measured(let percent, let truePercent, let note) = VoskWakeSpotter.assessment(of: "Hey Presto!") else {
            return XCTFail("hey presto is in the matrix and must report its rate")
        }
        XCTAssertEqual(percent, 33)
        XCTAssertTrue(note.contains("33%"), "the note must quote the measured number, not paraphrase it: \(note)")
        // **Both axes reach the sentence, and that is the point of the change.**
        // A note carrying only the false-accept number reads as praise for a
        // phrase that never fires.
        XCTAssertTrue(note.contains("\(truePercent)%"), "the note must quote the TRUE-accept number too: \(note)")
        XCTAssertTrue(
            note.contains("\(VoskWakeSpotter.syntheticSpeakerRenderings)"),
            "the note must name the denominator the true-accept rate was taken over: \(note)"
        )
        // Normalisation is what makes the table usable at all: the phrase reaching
        // this is the user's, punctuation and capitals included.
        XCTAssertEqual(VoskWakeSpotter.assessment(of: "  HEY   MAGICIAN  "),
                       VoskWakeSpotter.assessment(of: "hey magician"))
    }

    /// **The case that did not exist before.** A phrase built from a name nobody
    /// has run through the matrix is the ordinary production case — the matrix
    /// holds four names — and it used to be reported by returning nil, which is
    /// exactly how `hey magician` was reported.
    func testAnUnmeasuredPhraseIsReportedAsUnmeasuredRatherThanPassing() {
        for phrase in ["hey alexandra", "hey jarvis", "hey computer", "okay magican"] {
            guard case .unmeasured(let note) = VoskWakeSpotter.assessment(of: phrase) else {
                return XCTFail("\(phrase) is not in the matrix and must say so")
            }
            XCTAssertTrue(note.lowercased().contains("never been measured"), note)
        }
        // Distinguishable from a measured one — the requirement the old shape
        // could not meet, because both were nil.
        XCTAssertNotEqual(VoskWakeSpotter.assessment(of: "hey alexandra"),
                          VoskWakeSpotter.assessment(of: "hey magician"))
    }

    /// An unmeasured phrase still ARMS. Reporting is not refusing, and the note
    /// shape did not quietly become a gate.
    func testAnUnmeasuredPhraseStillArms() throws {
        spotter.configure(phrases: ["hey computer"])
        XCTAssertTrue(spotter.isArmed, "an unmeasured phrase must arm — the note is a report, not a refusal")
        XCTAssertEqual(spotter.rejectedPhrases, [])
        XCTAssertEqual(spotter.phraseNotes.map(\.phrase), ["hey computer"])
        XCTAssertTrue(spotter.phraseNotes[0].note.lowercased().contains("never been measured"))
    }

    /// The surviving rule, and the only shape rule the matrix supports: seven bare
    /// names were measured against their own prefixed form, five improved, one
    /// tied and one — `pico` — came out one utterance ahead. A single word the
    /// matrix has no number for still gets the class fact rather than silence,
    /// **and the note states the exception rather than rounding it away.**
    func testASingleWordWithNoNumberStillCarriesTheClassFinding() {
        guard case .bareWord(let note) = VoskWakeSpotter.assessment(of: "alexandra") else {
            return XCTFail("a bare word outside the matrix must report the class finding")
        }
        XCTAssertTrue(note.contains("13% to 80%"), note)
        XCTAssertTrue(note.contains("98% to 100%"), "the class fact must carry BOTH axes: \(note)")
        // The counterexample is named. A class claim with a known exception
        // hidden inside it is the shape of the retired letter-count rule.
        XCTAssertTrue(note.contains("cost one a single utterance"), note)
        // A bare word the matrix DOES have a number for reports the number, which
        // is strictly more than the class fact.
        XCTAssertEqual(
            VoskWakeSpotter.assessment(of: "sam"),
            .measured(nearMissFalseAcceptPercent: 80, syntheticTrueAcceptPercent: 100, note: VoskWakeSpotter.assessment(of: "sam").note!)
        )
    }

    /// Nothing survives normalisation, so there is no phrase to assess — the same
    /// answer `configure` gives, and for the same reason.
    func testAPhraseThatNormalisesToNothingIsUnarmable() {
        XCTAssertEqual(VoskWakeSpotter.assessment(of: "!!!"), .notInLexicon(unknownWords: []))
        XCTAssertNil(VoskWakeSpotter.assessment(of: "!!!").note)
    }

    /// Every number in the table has to be one the matrix produced, and the
    /// matrix reports fractions of 15. A percentage that is not a fifteenth is a
    /// number somebody typed rather than measured.
    func testEveryTabledRateIsAFifteenth() {
        for (phrase, percent) in VoskWakeSpotter.measuredNearMissFalseAcceptPercent {
            let hits = (0...15).first { Int(Double($0) / 15.0 * 100) == percent }
            XCTAssertNotNil(hits, "\(phrase) at \(percent)% is not n/15 — where did it come from?")
        }
    }

    /// The same guard on the other axis. Its denominator is the installed voice
    /// set rather than a literal 15, so the check reads it from the one place
    /// that records it — a table whose percentages stopped being n-of-that is a
    /// table that outlived the run it came from.
    func testEveryTabledTrueAcceptRateIsOverTheRecordedDenominator() {
        let n = VoskWakeSpotter.syntheticSpeakerRenderings
        XCTAssertGreaterThan(n, 0)
        for (phrase, rates) in VoskWakeSpotter.measured {
            let hits = (0...n).first { Int(Double($0) / Double(n) * 100) == rates.syntheticTrueAcceptPercent }
            XCTAssertNotNil(
                hits,
                "\(phrase) at \(rates.syntheticTrueAcceptPercent)% is not n/\(n) — where did it come from?"
            )
        }
    }

    /// **Neither axis may be recorded without the other.** The whole point of the
    /// pairing is that a false-accept number alone ranks a phrase that never
    /// fires first, so a row carrying one number and not the other would restore
    /// exactly the reading this table was restructured to prevent. One table
    /// makes that structurally impossible; this fails if it is ever split again.
    func testEveryMeasuredPhraseCarriesBothAxes() {
        XCTAssertFalse(VoskWakeSpotter.measured.isEmpty)
        XCTAssertEqual(
            Set(VoskWakeSpotter.measured.keys),
            Set(VoskWakeSpotter.measuredNearMissFalseAcceptPercent.keys),
            "the derived near-miss view has drifted from the table it is derived from"
        )
        for (phrase, rates) in VoskWakeSpotter.measured {
            guard case .measured(let falsePercent, let truePercent, _) = VoskWakeSpotter.assessment(of: phrase) else {
                return XCTFail("\(phrase) is in the table and must assess as measured")
            }
            XCTAssertEqual(falsePercent, rates.nearMissFalseAcceptPercent, phrase)
            XCTAssertEqual(truePercent, rates.syntheticTrueAcceptPercent, phrase)
        }
    }

    /// **The suspicion this axis was added to settle, pinned so it cannot quietly
    /// come back.** `hey samy` has the lowest near-miss rate ever measured (6%),
    /// and a phrase that never fires would score exactly that — indistinguishable
    /// on one axis. It fires on 47 of 50 synthetic renderings, so the 6% is
    /// discrimination and not deafness.
    ///
    /// Not a threshold: no phrase is refused or preferred on this number, and
    /// nothing reads it but the sentence in `assessment(of:)`. It is pinned
    /// because it is the specific fact that retired the specific suspicion.
    func testTheLowestFalseAcceptRowIsNotDeaf() {
        guard let samy = VoskWakeSpotter.measured["hey samy"] else {
            return XCTFail("hey samy left the table")
        }
        XCTAssertEqual(
            samy.nearMissFalseAcceptPercent,
            VoskWakeSpotter.measured.values.map(\.nearMissFalseAcceptPercent).min(),
            "hey samy is no longer the lowest near-miss row — re-check which row this case should be about"
        )
        XCTAssertGreaterThan(
            samy.syntheticTrueAcceptPercent, 50,
            "the best near-miss row woke on under half its own renderings — its 6% is deafness, not discrimination"
        )
        // And nothing in the matrix is deaf, which is the stronger result: the
        // near-miss column was not hiding this failure on ANY row.
        for (phrase, rates) in VoskWakeSpotter.measured {
            XCTAssertGreaterThan(rates.syntheticTrueAcceptPercent, 50, "\(phrase) woke on under half its own renderings")
        }
    }

    func testKnownWordsResolveAndUnknownDoesNot() throws {
        // Pins the assumption the rest of the suite rests on. `magican` is the real
        // brand word and the real problem; the others are the alternatives that
        // were on the table.
        spotter.configure(phrases: ["hey magician", "hey presto", "hey jarvis", "hey computer"])
        XCTAssertEqual(spotter.rejectedPhrases, [], "a candidate wake phrase left the lexicon")
        spotter.configure(phrases: ["magican"])
        XCTAssertEqual(spotter.rejectedPhrases, ["magican"])
    }

    // MARK: - configure / reset semantics

    func testConfigureReplacesRatherThanAppends() async throws {
        spotter.configure(phrases: [Self.phrase])
        let wokeBefore = await wakes(on: Self.phrase)
        XCTAssertTrue(wokeBefore)
        spotter.configure(phrases: ["hey computer"])
        XCTAssertEqual(spotter.rejectedPhrases, [])
        let stillWakes = await wakes(on: Self.phrase)
        XCTAssertFalse(stillWakes, "the previous window's phrase is still armed — configure appended")
    }

    /// A previous armed stretch must not be able to trigger the next one.
    ///
    /// The obvious shape — feed the first half, reset, feed the second half,
    /// expect silence — does not discriminate, and finding that out is itself
    /// part of the near-miss finding below: the grammar forces almost any tail
    /// containing the phrase's stressed syllables onto the phrase, so the second
    /// half fires on its own whether or not the reset worked. So the test feeds a
    /// prefix and then only SILENCE: a decoder that kept the prefix has a live
    /// partial that can still complete, and one that dropped it has nothing.
    func testResetDiscardsPriorAudio() async throws {
        spotter.configure(phrases: [Self.phrase])
        let pcm = await SpokenAudio.pcm16(Self.phrase)
        XCTAssertFalse(pcm.isEmpty)
        let prefix = (pcm.count * 2 / 5) & ~1  // keep the split on a whole PCM16 sample
        spotter.reset()
        hits = []
        feed(pcm.subdata(in: 0..<prefix))
        await settle()
        try XCTSkipIf(!hits.isEmpty, "the phrase fires on a 40% prefix — see testShortestPrefixThatFires")
        spotter.reset()
        feed(silence(Self.trailingSilenceMs))
        await settle()
        XCTAssertEqual(hits.count, 0, "audio from before reset() completed a phrase after it")
    }

    /// **How much of the phrase has to be spoken before it fires.** This is the
    /// mechanism behind the near-miss rate: the grammar's only path beginning
    /// with the first word ends in the last one, so once the opening syllables
    /// land the decoder's best in-progress hypothesis is already the whole
    /// phrase — and a partial is what the spotter reads. Reported rather than
    /// asserted; the fix (read finals, or require more of the phrase) is a
    /// latency-versus-accuracy product decision, not this task's to take.
    func testShortestPrefixThatFires() async throws {
        spotter.configure(phrases: [Self.phrase])
        let pcm = await SpokenAudio.pcm16(Self.phrase)
        XCTAssertFalse(pcm.isEmpty)
        var firstFiring: Int?
        var table: [String] = []
        for percent in stride(from: 10, through: 100, by: 10) {
            spotter.reset()
            hits = []
            let end = (pcm.count * percent / 100) & ~1
            feed(pcm.subdata(in: 0..<end))
            feed(silence(Self.trailingSilenceMs))
            await settle()
            let fired = !hits.isEmpty
            table.append("│   \(String(format: "%3d", percent))% (\(String(format: "%4d", end / 32)) ms) -> \(fired ? "FIRES" : "silent")")
            if fired, firstFiring == nil { firstFiring = percent }
        }
        print("""

        ┌── SHORTEST PREFIX THAT FIRES ────────────────────────────────────────
        │ phrase: "\(Self.phrase)" (\(pcm.count / 32) ms rendered)
        \(table.joined(separator: "\n"))
        │ first firing prefix: \(firstFiring.map { "\($0)%" } ?? "none")
        └──────────────────────────────────────────────────────────────────────

        """)
    }

    func testResetClearsTheFireCooldownSoTheNextWindowCanWakeImmediately() async throws {
        spotter.configure(phrases: [Self.phrase])
        let first = await wakes(on: Self.phrase)
        XCTAssertTrue(first, "precondition: the phrase wakes")
        // `wakes(on:)` resets first, which is exactly the path a re-arm takes.
        let second = await wakes(on: Self.phrase)
        XCTAssertTrue(second, "reset() left the fire cooldown standing")
    }

    /// Vosk endpoints on its own heuristics, so one stretch of speech can
    /// finalise more than once and the phrase can land in two consecutive
    /// utterances. Only the first may become a hit.
    func testRepeatedUtterancesFireOnlyOnceInsideTheCooldown() async throws {
        spotter.configure(phrases: [Self.phrase])
        spotter.reset()
        hits = []
        await speak(Self.phrase)
        await speak(Self.phrase)  // inside the 4 s fire cooldown
        XCTAssertEqual(hits.count, 1, "the fire cooldown did not suppress the repeat")
    }

    // MARK: - isolation

    /// `onHit` is typed `@MainActor`, and the hop is the spotter's obligation.
    /// Feeding from a background queue is what actually proves it: fed from the
    /// main thread, an implementation that never hops passes anyway.
    func testOnHitArrivesOnTheMainActorEvenWhenFedOffIt() async throws {
        spotter.configure(phrases: [Self.phrase])
        spotter.reset()
        hits = []
        let pcm = await SpokenAudio.pcm16(Self.phrase) + silence(Self.trailingSilenceMs)
        let spotter = self.spotter!
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                XCTAssertFalse(Thread.isMainThread, "precondition: feeding off the main thread")
                let frame = 1_600 * MemoryLayout<Int16>.size
                var offset = 0
                while offset < pcm.count {
                    let end = min(offset + frame, pcm.count)
                    spotter.feed(pcm.subdata(in: offset..<end))
                    offset = end
                }
                continuation.resume()
            }
        }
        await settle()
        XCTAssertEqual(hits.count, 1, "no hit from the background feed")
        XCTAssertEqual(hitWasOnMainThread, [true], "onHit was delivered off the main actor")
    }

    // MARK: - pure helpers

    func testGrammarAlwaysCarriesTheUnknownToken() {
        XCTAssertEqual(VoskWakeSpotter.grammarJSON(for: ["hey magician"]), #"["hey magician","[unk]"]"#)
        XCTAssertEqual(
            VoskWakeSpotter.grammarJSON(for: ["hey computer", "hey magician"]),
            #"["hey computer","hey magician","[unk]"]"#
        )
        XCTAssertEqual(VoskWakeSpotter.grammarJSON(for: []), #"["[unk]"]"#)
    }

    func testNormaliseMatchesTheLexiconShape() {
        XCTAssertEqual(VoskWakeSpotter.normalise("Hey, Magician!"), "hey magician")
        XCTAssertEqual(VoskWakeSpotter.normalise("  HEY   MAGICIAN  "), "hey magician")
        XCTAssertEqual(VoskWakeSpotter.normalise("what's up"), "what's up")
        XCTAssertEqual(VoskWakeSpotter.normalise("!!!"), "")
    }
}
