import AVFoundation
import CVosk
import XCTest

@testable import Magician

/// Accuracy measurements that inform an **open product decision**, kept apart
/// from `VoskWakeSpotterTests` because they assert almost nothing: they print
/// numbers for someone to choose from.
///
/// **These drive Vosk directly rather than through `VoskWakeSpotter`, on
/// purpose.** The question on the table — partials versus finalised results — is
/// a change to the spotter's behaviour, and adding a mode switch to production
/// so a test could flip it would be implementing half of the mitigation before
/// anyone decided to take it. Driving the engine here keeps production byte-for-
/// byte unchanged.
///
/// The cost of that choice is that the probe re-implements the spotter's decode
/// loop, so it could drift from the thing it claims to measure. That is paid for
/// two ways: the grammar, normalisation and result parsing are the spotter's own
/// `static` helpers rather than copies, and `testProbeReproducesTheSpotterBaseline`
/// pins the probe's partial-mode result against the rate
/// `VoskWakeSpotterTests.testNearMissFalseAcceptRate` measures through the real
/// type. A finals number is only worth having if the partial number it is
/// compared against came out of the same harness.
@MainActor
final class VoskWakeAccuracyMeasurementTests: XCTestCase {

    /// Whether a hit is taken from an in-progress hypothesis or only from a
    /// finalised utterance. `final` is what `VoskWakeSpotter` ships, as of
    /// 583c0570f; `partial` survives here only as the column the finals switch is
    /// measured against, and nothing in production reads it.
    enum MatchMode: String {
        case partial
        case final
    }

    /// A phrase and the three adversarial sets it actually has to survive.
    ///
    /// Per-phrase rather than one shared list, because the whole point of the
    /// exercise is that the rate is a property of the phrase — and for a prefixed
    /// phrase, of the prefix's own conversational frequency.
    struct Candidate {
        let phrase: String
        /// Phonetic neighbours of this phrase. 15 each, mirrored across the
        /// families so the columns are directly comparable.
        let nearMisses: [String]
        /// **Realistic sentences that open with the prefix and never reach the
        /// name.** This is the set that matters most for a conversational prefix:
        /// "listen" is a very common opener, and a generic unrelated-speech set
        /// scores 0/8 while exercising none of it.
        let openers: [String]
        /// The prefix itself mangled, with the real name intact — plus the
        /// non-adjacent case ("listen to presto"), which contains both words but
        /// must not match because the rule is a contiguous token run.
        let prefixConfusions: [String]
    }

    private static func listenOpeners() -> [String] {
        [
            "listen i'll call you back",
            "listen some of these need replacing",
            "listen to this",
            "i was listening to it earlier",
            "listen carefully to what i am about to say",
            "did you listen to the podcast",
            "she listened to the whole thing",
            "listen up everyone",
        ]
    }

    private static func heyOpeners() -> [String] {
        [
            "hey i'll call you back",
            "hey can you help me with something",
            "hey there",
            "hey what are you doing later",
            "hey did you see the news",
            "hey thanks for doing that",
            "hey come look at this",
            "hey everyone",
        ]
    }

    private static func listenConfusions(_ name: String) -> [String] {
        ["lesson \(name)", "listening \(name)", "listened \(name)", "glisten \(name)", "listen to \(name)"]
    }

    private static func heyConfusions(_ name: String) -> [String] {
        ["hay \(name)", "hi \(name)", "they \(name)", "say \(name)", "hey there \(name)"]
    }

    /// 15 each, and none of them contains its row's name as a whole token —
    /// otherwise the bare row would be counting true matches as false accepts and
    /// the column would not be comparable. (This is why "a magician" / "the
    /// magician" are not here: they are legitimate matches for bare `magician`.)
    private static let magicianNeighbours = [
        "musician", "machine", "position", "magic", "magazine", "mission", "physician",
        "magnesium", "imagine", "mechanic", "message", "managing", "medicine",
        "magicians", "magical",
    ]
    private static let prestoNeighbours = [
        "pesto", "preston", "espresso", "press", "protest", "prestige", "president",
        "pressure", "priest", "impressed", "expressed", "rest", "best", "test", "west",
    ]
    private static let samNeighbours = [
        "sand", "same", "sat", "some", "sum", "slam", "spam", "swam", "seem",
        "psalm", "ham", "jam", "ram", "damn", "samantha",
    ]
    /// Neighbours for the two-syllable forms of Sam, built to the same recipe as
    /// `samNeighbours` so the rows are comparable rather than merely adjacent:
    /// the shorter form itself ("sam", the deletion case that "pesto" is for
    /// "presto"), words that begin with the name and continue ("samba", "sandy",
    /// "salmon", "salami", "samurai", "samantha" — "samantha" kept from the `sam`
    /// row on purpose), vowel and consonant substitutions ("semi", "sunny",
    /// "savvy"), and five rhymes on `-ammy` with a different onset ("clammy",
    /// "grammy", "whammy", "jammy", "tammy") standing in for `sam`'s
    /// "ham"/"jam"/"ram"/"damn". 15, like every other family.
    ///
    /// Near-misses are SPOKEN, so none of these has to be in the lexicon — only
    /// pronounceable. And none of them contains any of `samy`/`sammy`/`sammie`
    /// as a whole token, which is why the deliberate homophone spellings are NOT
    /// in here: "sammie" spoken is "sammy" heard, so it would be a true match
    /// counted as a false accept.
    private static let sammyNeighbours = [
        "sam", "samba", "sandy", "salmon", "salami", "samurai", "samantha",
        "semi", "sunny", "savvy", "clammy", "grammy", "whammy", "jammy", "tammy",
    ]
    /// Neighbours for `pico`, to the same recipe and the same size as every other
    /// family, so the row is comparable rather than merely present: the truncation
    /// cases ("peak", "pick" — what "pesto" is to "presto"), words that begin like
    /// the name and continue ("piccolo", "picnic", "picasso", "pixel", "portico"),
    /// vowel and consonant substitutions ("psycho", "pecan", "peso", "taco"), and
    /// four rhymes on the `-co` ending with a different onset ("disco", "echo",
    /// "chico", "mexico").
    ///
    /// None of them contains `pico` as a whole token, which is the rule that keeps
    /// the column a false-accept column: "portico" and "piccolo" both *contain*
    /// the letters and neither is the token, and the match rule is a contiguous
    /// whole-token run.
    private static let picoNeighbours = [
        "peak", "pick", "piccolo", "picnic", "picasso", "pixel", "portico",
        "psycho", "pecan", "peso", "taco", "disco", "echo", "chico", "mexico",
    ]

    private static let candidates: [Candidate] = [
        // Bare `magician` is the control the matrix was missing: without it there
        // is no way to tell whether `hey magician`'s 13% comes from the prefix or
        // from `magician` simply being a long, phonetically uncrowded word.
        Candidate(phrase: "magician", nearMisses: magicianNeighbours, openers: [], prefixConfusions: []),
        Candidate(
            phrase: "hey magician", nearMisses: magicianNeighbours.map { "hey \($0)" },
            openers: heyOpeners(), prefixConfusions: heyConfusions("magician")
        ),
        Candidate(phrase: "presto", nearMisses: prestoNeighbours, openers: [], prefixConfusions: []),
        Candidate(
            phrase: "hey presto", nearMisses: prestoNeighbours.map { "hey \($0)" },
            openers: heyOpeners(), prefixConfusions: heyConfusions("presto")
        ),
        Candidate(phrase: "sam", nearMisses: samNeighbours, openers: [], prefixConfusions: []),
        Candidate(
            phrase: "hey sam", nearMisses: samNeighbours.map { "hey \($0)" },
            openers: heyOpeners(), prefixConfusions: heyConfusions("sam")
        ),
        // The owner's proposal: "listen" is two syllables and less phonetically
        // crowded than "hey", so it should give the decoder real acoustic material
        // before the name arrives.
        Candidate(
            phrase: "listen presto", nearMisses: prestoNeighbours.map { "listen \($0)" },
            openers: listenOpeners(), prefixConfusions: listenConfusions("presto")
        ),
        Candidate(
            phrase: "listen sam", nearMisses: samNeighbours.map { "listen \($0)" },
            openers: listenOpeners(), prefixConfusions: listenConfusions("sam")
        ),
        // Separates "a longer PREFIX helps" from "a longer PHRASE helps": if
        // `listen` is doing the work, this should beat `hey magician`; if the
        // distinctive NAME is doing it, this should barely move.
        Candidate(
            phrase: "listen magician",
            nearMisses: magicianNeighbours.map { "listen \($0)" },
            openers: listenOpeners(), prefixConfusions: listenConfusions("magician")
        ),
        // **Is a LONGER Sam a better Sam?** `sam` is one syllable and the worst
        // row in the matrix (80% bare, 53% prefixed); the owner's question is
        // whether a second syllable rescues it. Three spellings are measured
        // rather than one because they are the same AUDIO through different
        // lexicon entries — the near-miss set is shared verbatim across all three
        // — so the spread between these rows is a measurement of the model's
        // pronunciation for each spelling and nothing else. `listen` is skipped:
        // it is already established as worse than `hey` on every name measured.
        Candidate(phrase: "samy", nearMisses: sammyNeighbours, openers: [], prefixConfusions: []),
        Candidate(
            phrase: "hey samy", nearMisses: sammyNeighbours.map { "hey \($0)" },
            openers: heyOpeners(), prefixConfusions: heyConfusions("samy")
        ),
        Candidate(phrase: "sammy", nearMisses: sammyNeighbours, openers: [], prefixConfusions: []),
        Candidate(
            phrase: "hey sammy", nearMisses: sammyNeighbours.map { "hey \($0)" },
            openers: heyOpeners(), prefixConfusions: heyConfusions("sammy")
        ),
        Candidate(phrase: "sammie", nearMisses: sammyNeighbours, openers: [], prefixConfusions: []),
        Candidate(
            phrase: "hey sammie", nearMisses: sammyNeighbours.map { "hey \($0)" },
            openers: heyOpeners(), prefixConfusions: heyConfusions("sammie")
        ),
        // **The owner asked about `tobo` and `pico`. Only one of them is here.**
        // `tobo` is not in the model lexicon (`vosk_model_find_word` -> -1), so it
        // cannot be armed at all and any rate measured for it would be a rate for
        // a silently shortened grammar — see
        // `testToboIsRefusedWholeRatherThanMeasured`. `pico` resolves to 87542 and
        // therefore gets ordinary rows.
        Candidate(phrase: "pico", nearMisses: picoNeighbours, openers: [], prefixConfusions: []),
        Candidate(
            phrase: "hey pico", nearMisses: picoNeighbours.map { "hey \($0)" },
            openers: heyOpeners(), prefixConfusions: heyConfusions("pico")
        ),
    ]

    // MARK: - synthetic speaker variation

    /// Prosody settings applied to every voice: the utterance defaults, then one
    /// step out on each of rate and pitch in each direction.
    ///
    /// A five-point star rather than a full 3x3 cross, so the rendering count per
    /// phrase stays proportional to the voice count. `nil` means "leave
    /// `AVSpeechUtterance`'s own default", which keeps the first cell on exactly
    /// the code path every near-miss clip in this file is rendered by.
    /// ±20% on rate is a legible change in pace without the clipped diction the
    /// extremes of the range produce; ±15% on pitch moves timbre without
    /// chipmunking it.
    private static let prosodyGrid: [(rate: Float?, pitch: Float?)] = [
        (nil, nil),
        (AVSpeechUtteranceDefaultSpeechRate * 0.8, nil),
        (AVSpeechUtteranceDefaultSpeechRate * 1.2, nil),
        (nil, 0.85),
        (nil, 1.15),
    ]

    /// Every (installed English voice x prosody setting) pair, built at run time.
    ///
    /// **The count is not a constant and must never be written down as one.**
    /// It is whatever this simulator image happens to have installed, which is
    /// the caveat that has to travel with every true-accept percentage this file
    /// produces.
    private static func speakers() -> [SpokenAudio.Speaker] {
        SpokenAudio.englishVoices().flatMap { voice in
            prosodyGrid.map { prosody in
                SpokenAudio.Speaker(
                    voiceIdentifier: voice.identifier, name: voice.name,
                    language: voice.language, rate: prosody.rate, pitch: prosody.pitch
                )
            }
        }
    }

    /// **What the true-accept denominator actually is on this machine.**
    ///
    /// Printed rather than asserted against a number, because the installed voice
    /// set is a property of the simulator image. The one thing asserted is that
    /// there IS a set and that every member of it renders audio — a voice that
    /// silently renders nothing would drag every true-accept rate down uniformly
    /// and look like a phrase finding.
    func testSyntheticSpeakerInventory() async throws {
        let voices = SpokenAudio.englishVoices()
        let all = AVSpeechSynthesisVoice.speechVoices()
        let otherLanguages: [String] = Set(all.map(\.language))
            .subtracting(SpokenAudio.englishLocales).sorted()
        let excluded = SpokenAudio.allEnglishVoices().filter(SpokenAudio.isExcluded)
        var rows: [String] = []
        for voice in voices {
            var lengths: [String] = []
            for prosody in Self.prosodyGrid {
                let speaker = SpokenAudio.Speaker(
                    voiceIdentifier: voice.identifier, name: voice.name,
                    language: voice.language, rate: prosody.rate, pitch: prosody.pitch
                )
                let clip = await SpokenAudio.pcm16("hey presto", as: speaker)
                lengths.append(clip.isEmpty ? "EMPTY" : "\(clip.count / Self.bytesPerMillisecond)ms")
            }
            let head = Self.pad(voice.language, 8) + Self.pad(voice.name, 12)
            rows.append("│   " + head + lengths.joined(separator: "  "))
        }
        print("""

        ┌── SYNTHETIC SPEAKERS INSTALLED ──────────────────────────────────────
        │ voices on this image: \(all.count) total, \(SpokenAudio.allEnglishVoices().count) in \
        \(SpokenAudio.englishLocales.joined(separator: "/")), \(voices.count) after excluding novelty
        │ prosody settings per voice: \(Self.prosodyGrid.count)  →  \(Self.speakers().count) renderings per phrase
        │ (columns: "hey presto" at default, slow, fast, low, high)
        \(rows.joined(separator: "\n"))
        │
        │ EXCLUDED as novelty/personal (\(excluded.count)): \(excluded.map(\.name).joined(separator: ", "))
        │ non-English voices present: \(otherLanguages.joined(separator: " "))
        └──────────────────────────────────────────────────────────────────────

        """)
        XCTAssertFalse(voices.isEmpty, "no English voices installed — every true-accept rate would be 0/0")
        // The denominator recorded in `VoskWakeSpotter` has to be the one this
        // machine produces, or the table carries a number from a different image.
        XCTAssertEqual(
            Self.speakers().count, VoskWakeSpotter.syntheticSpeakerRenderings,
            """
            The installed voice set changed: this image gives \(Self.speakers().count) renderings per \
            phrase and VoskWakeSpotter.syntheticSpeakerRenderings says \
            \(VoskWakeSpotter.syntheticSpeakerRenderings). Every true-accept percentage in that table \
            was taken over a different denominator — re-run testAccuracyMatrix and re-record.
            """
        )
    }

    /// Ordinary speech, shared across phrases — the `[unk]` control.
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

    private static let sampleRate: Float = 16_000
    /// 16 kHz mono PCM16 → 32 bytes per millisecond.
    private static let bytesPerMillisecond = 32

    /// One model for the whole class. Loading is the expensive part; recognizers
    /// are built and thrown away per probe, which is what the spotter does too.
    private static var model: OpaquePointer?

    override func setUp() async throws {
        try await super.setUp()
        try XCTSkipIf(VoskWakeSpotter.bundledModelURL == nil,
                      "No bundled Vosk model — run `make setup-magios-vosk` and regenerate.")
        if Self.model == nil {
            vosk_set_log_level(-1)
            Self.model = vosk_model_new(VoskWakeSpotter.bundledModelURL!.path)
        }
        XCTAssertNotNil(Self.model, "could not load the Vosk model")
    }

    // MARK: - the probe

    /// Feed `audio` (plus trailing silence) frame by frame and report the byte
    /// offset at which the phrase matched, or nil.
    ///
    /// Mirrors `VoskWakeSpotter.feed` exactly apart from the one line under test:
    /// `partial` reads the in-progress hypothesis, `final` waits for the decoder
    /// to declare the utterance over.
    private func firingOffset(
        phrase: String, audio: Data, mode: MatchMode, trailingSilenceMs: Int = 1_500
    ) -> Int? {
        firingOffset(phrase: phrase, mode: mode, segments: [
            audio, Data(count: trailingSilenceMs * Self.bytesPerMillisecond),
        ])
    }

    /// Feed `segments` frame by frame and report the byte offset at which the
    /// phrase matched, or nil.
    ///
    /// **Each segment is chunked independently**, which is not a detail: it is
    /// what `VoskWakeSpotterTests.speak` does when it feeds the utterance and
    /// then the trailing silence in two `feed` calls, and concatenating first
    /// moves the speech/silence boundary into the middle of a 100 ms frame.
    /// Vosk's endpointing is sensitive to that — it was worth exactly one case
    /// ("hey position") between this probe and the real type, which is precisely
    /// the drift `testProbeReproducesTheSpotterBaseline` exists to catch.
    ///
    /// Otherwise this mirrors `VoskWakeSpotter.feed` exactly apart from the one
    /// line under test: `partial` reads the in-progress hypothesis, `final` waits
    /// for the decoder to declare the utterance over.
    private func firingOffset(phrase: String, mode: MatchMode, segments: [Data]) -> Int? {
        guard let model = Self.model else { return nil }
        let normalised = VoskWakeSpotter.normalise(phrase)
        let grammar = VoskWakeSpotter.grammarJSON(for: [normalised])
        guard let recognizer = vosk_recognizer_new_grm(model, Self.sampleRate, grammar) else { return nil }
        defer { vosk_recognizer_free(recognizer) }

        let needle = normalised.split(separator: " ").map(String.init)
        let frame = 1_600 * MemoryLayout<Int16>.size  // 100 ms, as the mic delivers
        var consumed = 0
        for segment in segments {
            var offset = 0
            while offset < segment.count {
                let end = min(offset + frame, segment.count)
                let chunk = segment.subdata(in: offset..<end)
                let count = chunk.count / MemoryLayout<Int16>.size
                let samples = [Int16](unsafeUninitializedCapacity: count) { buffer, initialised in
                    _ = chunk.copyBytes(to: UnsafeMutableRawBufferPointer(buffer),
                                        count: count * MemoryLayout<Int16>.size)
                    initialised = count
                }
                let finished = samples.withUnsafeBufferPointer {
                    vosk_recognizer_accept_waveform_s(recognizer, $0.baseAddress, Int32(count))
                }
                let json: UnsafePointer<CChar>?
                switch mode {
                case .partial:
                    json = finished == 1
                        ? vosk_recognizer_result(recognizer)
                        : vosk_recognizer_partial_result(recognizer)
                case .final:
                    json = finished == 1 ? vosk_recognizer_result(recognizer) : nil
                }
                if let json,
                   let heard = VoskWakeSpotter.text(fromResultJSON: json),
                   Self.contains(needle, in: heard) {
                    return consumed + end
                }
                offset = end
            }
            consumed += segment.count
        }
        return nil
    }

    /// Contiguous whole-token run — the same rule `VoskWakeSpotter.matchedPhrase`
    /// applies, and the reason "hey samantha" must not match "sam".
    private static func contains(_ needle: [String], in heard: String) -> Bool {
        let tokens = heard.split(separator: " ").map(String.init)
        guard !needle.isEmpty, needle.count <= tokens.count else { return false }
        for start in 0...(tokens.count - needle.count)
        where Array(tokens[start..<(start + needle.count)]) == needle {
            return true
        }
        return false
    }

    /// Trailing silence used for every matrix cell.
    ///
    /// The same value for BOTH modes, which is what makes the partial and final
    /// columns comparable: `final` cannot fire at all without enough silence for
    /// the decoder to end the utterance, so measuring it against a partial column
    /// that had been given less would flatter it. 1.5 s is also the more honest
    /// question for a false accept — an armed window runs for minutes, so silence
    /// always arrives eventually, and "does this utterance EVER wake it" is what
    /// matters rather than "does it wake it within 400 ms".
    private static let matrixSilenceMs = 1_500

    /// What `VoskWakeSpotterTests` feeds after an utterance. Same as the matrix's
    /// now: the shipping type fires on finalised results, so its suite needs a
    /// finalising tail too.
    private static let spotterSuiteSilenceMs = 1_500

    private func falseAccepts(
        _ candidate: Candidate, mode: MatchMode, silenceMs: Int = 1_500
    ) async -> [String] {
        var accepted: [String] = []
        for miss in candidate.nearMisses {
            let audio = await SpokenAudio.pcm16(miss)
            guard !audio.isEmpty else { continue }
            if firingOffset(phrase: candidate.phrase, audio: audio, mode: mode,
                            trailingSilenceMs: silenceMs) != nil {
                accepted.append(miss)
            }
        }
        return accepted
    }

    /// One phrase's TRUE-ACCEPT axis: how many of `speakers` say the phrase in a
    /// way that actually wakes it.
    ///
    /// ## What this approximates, and what it does not
    ///
    /// It is **synthetic speaker variation, not human speech.** Every rendering
    /// comes out of one `AVSpeechSynthesizer` on one OS image; the voices differ
    /// in timbre and accent and the prosody grid moves pace and pitch, but they
    /// share a pronunciation model, an idealised articulation, a studio-clean
    /// channel and no room, no distance, no background, no cold, no mouth full of
    /// coffee, and no disfluency. A real speaker varies along axes this cannot
    /// reach at all.
    ///
    /// So the number it produces is **a lower bound on the problem, not a
    /// measurement of the real-world wake rate.** A phrase that fails to wake on
    /// clean synthetic speech will certainly fail worse on a person, which is the
    /// direction that matters: this catches deafness, and does not certify
    /// hearing. A high number here is the absence of one specific failure, not
    /// evidence the phrase works — do not quote it as "wakes N% of the time".
    ///
    /// ## Why it exists at all
    ///
    /// Because the near-miss column alone is one-sided in the worst possible way:
    /// **a phrase that never fires scores a perfect 0% false accepts**, which is
    /// indistinguishable by eye from the best phrase ever measured. That is the
    /// same shape as the deadlocked-synthesiser bug this file's audio census was
    /// added for — a flawless zero produced by measuring nothing. `hey samy`'s
    /// 6% was exactly this suspicion, and this axis is what settles it.
    ///
    /// Measuring one rendering of the phrase against itself would not: `hey samy`
    /// already fires on ~70% of its own single default rendering, so the
    /// self-test passes for a phrase that no other voice can reach. **The
    /// variation IS the measurement.**
    private func trueAccepts(
        _ phrase: String, speakers: [SpokenAudio.Speaker]
    ) async -> (fired: Int, rendered: Int, missedBy: [String], totalMs: Int) {
        var fired = 0
        var rendered = 0
        var missedBy: [String] = []
        var totalMs = 0
        for speaker in speakers {
            let audio = await SpokenAudio.pcm16(phrase, as: speaker)
            guard !audio.isEmpty else { continue }
            rendered += 1
            totalMs += audio.count / Self.bytesPerMillisecond
            if firingOffset(phrase: phrase, audio: audio, mode: .final,
                            trailingSilenceMs: Self.matrixSilenceMs) != nil {
                fired += 1
            } else {
                missedBy.append(speaker.label)
            }
        }
        return (fired, rendered, missedBy, totalMs)
    }

    // MARK: - harness validation

    /// The probe must reproduce what the real type measures, or none of the
    /// numbers below mean anything.
    ///
    /// Validated against the SHIPPING mode, which is now `final` — so this pins
    /// the probe's final column, the one the phrase matrix is read from, rather
    /// than a partial column nothing ships. It has already earned its keep once:
    /// it disagreed by a single case, and the cause was frame alignment
    /// (`firingOffset(phrase:mode:segments:)` explains it).
    func testProbeReproducesTheSpotterBaseline() async throws {
        // VoskWakeSpotterTests' own list, copied explicitly rather than reused from
        // `candidates`: the matrix's sets are built uniformly across rows so the
        // grid is comparable, while that suite's set is the original near-miss
        // list. Pinning against it means pinning the exact thing it measures.
        let spotterSuiteNearMisses = [
            "hey musician", "hey machine", "hey position", "hey magic", "hey magazine",
            "hey mission", "hey physician", "hey magnesium", "a magician", "the magician",
            "hey imagine", "hey mechanic", "hey message", "hey managing", "hey medicine",
        ]
        let final = await accepts(spotterSuiteNearMisses, phrase: "hey magician", mode: .final)
        let partial = await accepts(spotterSuiteNearMisses, phrase: "hey magician", mode: .partial)
        print("probe \"hey magician\": final \(final.count)/15 (shipping), partial \(partial.count)/15 (not shipped)")
        XCTAssertEqual(
            final.count, 2,
            "the probe must reproduce VoskWakeSpotterTests.testNearMissFalseAcceptRate (2/15 on finals) — otherwise it has drifted from the thing it measures"
        )
    }

    /// Every phrase must be in the lexicon, or its row below is measuring a
    /// silently shortened grammar rather than the phrase.
    ///
    /// This is the check that makes the whole matrix trustworthy: a dropped word
    /// shortens the grammar with only a log line, so `listen presto` losing
    /// "listen" would quietly become a measurement of bare `presto` and every
    /// conclusion drawn from the row would be about the wrong phrase.
    func testEveryMeasuredPhraseIsInLexicon() throws {
        let spotter = VoskWakeSpotter()
        spotter.configure(phrases: Self.candidates.map(\.phrase))
        XCTAssertEqual(spotter.rejectedPhrases, [], "a measured phrase is not in the model lexicon")

        // Word by word too, and printed, because the per-phrase check above can
        // only say THAT a phrase failed, not which word cost it.
        guard let model = Self.model else { return XCTFail("no model") }
        var report: [String] = []
        // `samy`/`sammy`/`sammie` are here as the GATE on the rows below them:
        // a longer Sam that is not in the lexicon cannot be armed at all, and an
        // absent spelling would arm `hey samy` as bare "hey".
        for word in ["listen", "presto", "sam", "magician", "hey",
                     "samy", "sammy", "sammie", "tobo", "pico",
                     "computer", "jarvis", "magican"] {
            report.append("│   \(word.padding(toLength: 10, withPad: " ", startingAt: 0)) -> \(vosk_model_find_word(model, word))")
        }
        print("""

        ┌── LEXICON (vosk_model_find_word; -1 = absent) ───────────────────────
        \(report.joined(separator: "\n"))
        └──────────────────────────────────────────────────────────────────────

        """)
        for word in ["listen", "presto", "sam", "magician", "samy", "sammy", "sammie", "pico"] {
            XCTAssertGreaterThanOrEqual(vosk_model_find_word(model, word), 0, "\(word) must be in the lexicon")
        }
    }

    /// **`tobo` has no rates, and the reason is the only thing worth reporting
    /// about it.**
    ///
    /// It was proposed alongside `pico`; `pico` is in the lexicon and gets rows in
    /// the matrix, and `tobo` is not and cannot get any. This is a real assertion
    /// rather than a note in a doc because the failure it guards is silent: Vosk
    /// drops an out-of-lexicon word from a grammar with nothing but a log line, so
    /// `hey tobo` would arm as the bare phrase `hey` and fire on any sentence
    /// containing it. A near-miss rate measured on that grammar would be a
    /// perfectly real-looking number about the wrong phrase.
    ///
    /// **Nothing here predicts lexicon membership from the shape of the word**,
    /// and the class of mistake that would be is on record: `samy` was expected to
    /// be absent and resolves to 75756. Both of these were looked up.
    func testToboIsRefusedWholeRatherThanMeasured() async throws {
        guard let model = Self.model else { return XCTFail("no model") }
        XCTAssertEqual(vosk_model_find_word(model, "tobo"), -1,
                       "`tobo` entered the lexicon — it can now be measured, so give it matrix rows")
        XCTAssertGreaterThanOrEqual(vosk_model_find_word(model, "pico"), 0,
                                    "`pico` left the lexicon — its matrix rows are now measuring a shortened grammar")

        let spotter = VoskWakeSpotter()
        spotter.configure(phrases: ["hey tobo", "tobo"])
        XCTAssertEqual(spotter.rejectedPhrases, ["hey tobo", "tobo"])
        XCTAssertFalse(spotter.isArmed, "an out-of-lexicon name must not arm a shortened grammar")
        // **`assessment(of:)` cannot see this and must not be asked to.** It is a
        // pure function over the string — no model, no `vosk_model_find_word` —
        // which is what lets `SettingsView` show a note before anything is armed.
        // The lexicon refusal is `configure`'s alone, asserted above. An earlier
        // draft of this case expected `.notInLexicon` here and was wrong.
        XCTAssertEqual(
            VoskWakeSpotter.assessment(of: "hey tobo").note,
            VoskWakeSpotter.assessment(of: "hey alexandra").note,
            "assessment(of:) has started consulting the lexicon — it cannot, and SettingsView calls it with no model loaded"
        )

        // **The counterfactual, measured rather than asserted.** The claim this
        // refusal rests on is that a dropped word leaves a live shortened grammar,
        // so `hey tobo` would arm as bare `hey`. What that costs is a measurement,
        // and it came back smaller than the standing claim: on FINALS the bare
        // `hey` grammar fires on none of the eight conversational openers.
        var wokeFinal: [String] = []
        var wokePartial: [String] = []
        for opener in Self.heyOpeners() {
            let audio = await SpokenAudio.pcm16(opener)
            XCTAssertFalse(audio.isEmpty, "\"\(opener)\" rendered EMPTY — this check is measuring silence")
            if firingOffset(phrase: "hey", audio: audio, mode: .final,
                            trailingSilenceMs: Self.matrixSilenceMs) != nil {
                wokeFinal.append(opener)
            }
            if firingOffset(phrase: "hey", audio: audio, mode: .partial,
                            trailingSilenceMs: Self.matrixSilenceMs) != nil {
                wokePartial.append(opener)
            }
        }
        // What IS asserted: the shortened grammar is live, so the refusal is doing
        // real work rather than guarding a grammar that could never fire anyway.
        // Without this the 0/8 above would be ambiguous between "shortening is
        // harmless" and "this probe measures nothing".
        let bareHey = await SpokenAudio.pcm16("hey")
        XCTAssertFalse(bareHey.isEmpty, "\"hey\" rendered EMPTY — the liveness check is measuring silence")
        XCTAssertNotNil(
            firingOffset(phrase: "hey", audio: bareHey, mode: .final,
                         trailingSilenceMs: Self.matrixSilenceMs),
            """
            The grammar `hey` did not fire on the word "hey" itself, so this whole case is measuring \
            a dead probe rather than a live shortened grammar. Fix the probe before reading the 0/8.
            """
        )

        print("""

        ┌── `tobo` IS NOT IN THE LEXICON ──────────────────────────────────────
        │ vosk_model_find_word(tobo) = \(vosk_model_find_word(model, "tobo"))  → refused whole, no rates exist
        │ vosk_model_find_word(pico) = \(vosk_model_find_word(model, "pico"))  → measured, see the matrix
        │
        │ Counterfactual — had `tobo` been DROPPED instead of the phrase refused,
        │ the surviving grammar would be bare `hey`. Fed the 8 conversational
        │ openers, that grammar woke on:
        │   finals  (ships): \(wokeFinal.count)/8  \(wokeFinal.isEmpty ? "(none)" : wokeFinal.joined(separator: " | "))
        │   partials       : \(wokePartial.count)/8  \(wokePartial.isEmpty ? "(none)" : wokePartial.joined(separator: " | "))
        │ and on the word "hey" spoken alone: FIRES (the probe is live)
        └──────────────────────────────────────────────────────────────────────

        """)
    }

    // MARK: - the matrix

    private static func pad(_ text: String, _ width: Int) -> String {
        text.count >= width ? text : text + String(repeating: " ", count: width - text.count)
    }

    private static func rate(_ hits: Int, _ total: Int) -> String {
        total == 0 ? "  n/a  " : pad("\(hits)/\(total) (\(Int(Double(hits) / Double(total) * 100))%)", 11)
    }

    /// Probe a set of sentences and return the ones that woke the phrase.
    private func accepts(_ sentences: [String], phrase: String, mode: MatchMode) async -> [String] {
        var accepted: [String] = []
        for sentence in sentences {
            let audio = await SpokenAudio.pcm16(sentence)
            guard !audio.isEmpty else { continue }
            if firingOffset(phrase: phrase, audio: audio, mode: mode,
                            trailingSilenceMs: Self.matrixSilenceMs) != nil {
                accepted.append(sentence)
            }
        }
        return accepted
    }

    /// Fraction of the phrase's own audio needed before it fires.
    private func firingFraction(_ phrase: String, audio: Data, mode: MatchMode) -> String {
        for percent in stride(from: 10, through: 100, by: 10) {
            let end = (audio.count * percent / 100) & ~1
            if firingOffset(phrase: phrase, audio: audio.subdata(in: 0..<end), mode: mode,
                            trailingSilenceMs: mode == .final ? Self.matrixSilenceMs : 0) != nil {
                return "\(percent)%"
            }
        }
        return "—"
    }

    func testAccuracyMatrix() async throws {
        var accuracy: [String] = []
        var finalRateByPhrase: [String: String] = [:]
        var adversarial: [String] = []
        var detail: [String] = []
        var scorecard: [(phrase: String, falsePercent: Int, truePercent: Int)] = []

        let speakers = Self.speakers()
        XCTAssertFalse(speakers.isEmpty, "no synthetic speakers — every true-accept rate would be 0/0")

        for candidate in Self.candidates {
            let phrase = candidate.phrase
            let audio = await SpokenAudio.pcm16(phrase)
            let durationMs = audio.count / Self.bytesPerMillisecond

            let nearPartial = await accepts(candidate.nearMisses, phrase: phrase, mode: .partial)
            let nearFinal = await accepts(candidate.nearMisses, phrase: phrase, mode: .final)
            let unrelatedFinal = await accepts(Self.unrelated, phrase: phrase, mode: .final)
            let openersPartial = await accepts(candidate.openers, phrase: phrase, mode: .partial)
            let openersFinal = await accepts(candidate.openers, phrase: phrase, mode: .final)
            let confusionsFinal = await accepts(candidate.prefixConfusions, phrase: phrase, mode: .final)

            // **Proof the row was actually fed audio.** `accepts` skips a clip the
            // synthesiser declined to render, but the denominator is the set's
            // size either way — so a row whose fixtures all came back empty scores
            // a flawless 0/15, and a flawless zero is indistinguishable from a
            // silent one by eye. Re-reads are cache hits, so this costs nothing.
            var rendered = 0
            var totalMs = 0
            var shortestMs = Int.max
            for miss in candidate.nearMisses {
                let clip = await SpokenAudio.pcm16(miss)
                guard !clip.isEmpty else { continue }
                rendered += 1
                let ms = clip.count / Self.bytesPerMillisecond
                totalMs += ms
                shortestMs = min(shortestMs, ms)
            }
            XCTAssertEqual(
                rendered, candidate.nearMisses.count,
                "\(phrase): \(candidate.nearMisses.count - rendered) near-miss clips rendered EMPTY — this row's rate is measured against silence and means nothing"
            )

            // **The other axis, through the same census guard.** A true-accept
            // row fed silence would report a flat 0% and read as the strongest
            // possible finding about the phrase, which is precisely the failure
            // mode this whole axis exists to close on the false-accept side.
            let woke = await trueAccepts(phrase, speakers: speakers)
            XCTAssertEqual(
                woke.rendered, speakers.count,
                "\(phrase): \(speakers.count - woke.rendered) of \(speakers.count) speaker renderings came back EMPTY — this row's TRUE-accept rate is measured against silence and means nothing"
            )
            XCTAssertGreaterThan(
                woke.totalMs, 0,
                "\(phrase): every speaker rendering was zero-length — see the audio census"
            )
            let truePercent = Int(Double(woke.fired) / Double(max(woke.rendered, 1)) * 100)
            let falsePercent = Int(Double(nearFinal.count) / Double(candidate.nearMisses.count) * 100)
            scorecard.append((phrase, falsePercent, truePercent))

            finalRateByPhrase[phrase] = "\(nearFinal.count)/\(candidate.nearMisses.count) (\(falsePercent)%)"
            accuracy.append(
                "│ " + Self.pad(phrase, 16)
                + "│ " + Self.rate(nearPartial.count, candidate.nearMisses.count)
                + "│ " + Self.rate(nearFinal.count, candidate.nearMisses.count)
                + "│ " + Self.pad(firingFraction(phrase, audio: audio, mode: .partial), 8)
                + "│ " + Self.pad(firingFraction(phrase, audio: audio, mode: .final), 7)
                + "│ " + Self.pad("\(durationMs) ms", 8) + "│"
            )
            adversarial.append(
                "│ " + Self.pad(phrase, 16)
                + "│ " + Self.rate(unrelatedFinal.count, Self.unrelated.count)
                + "│ " + Self.rate(openersPartial.count, candidate.openers.count)
                + "│ " + Self.rate(openersFinal.count, candidate.openers.count)
                + "│ " + Self.rate(confusionsFinal.count, candidate.prefixConfusions.count) + "│"
            )
            detail.append("│ \(phrase):")
            detail.append("│   audio: \(rendered)/\(candidate.nearMisses.count) near-miss clips rendered, \(totalMs) ms total, shortest \(rendered == 0 ? 0 : shortestMs) ms; phrase \(durationMs) ms")
            detail.append("│   audio: \(woke.rendered)/\(speakers.count) speaker renderings, \(woke.totalMs) ms total")
            detail.append("│   TRUE ACCEPT: \(woke.fired)/\(woke.rendered) (\(truePercent)%) woke it")
            if !woke.missedBy.isEmpty {
                detail.append("│   DEAF TO     : \(woke.missedBy.joined(separator: " | "))")
            }
            detail.append("│   near-miss (final): \(nearFinal.isEmpty ? "(none)" : nearFinal.joined(separator: ", "))")
            if !candidate.openers.isEmpty {
                detail.append("│   OPENERS  (final): \(openersFinal.isEmpty ? "(none)" : openersFinal.joined(separator: " | "))")
            }
            if !candidate.prefixConfusions.isEmpty {
                detail.append("│   prefix   (final): \(confusionsFinal.isEmpty ? "(none)" : confusionsFinal.joined(separator: ", "))")
            }
            if !unrelatedFinal.isEmpty {
                detail.append("│   UNRELATED(final): \(unrelatedFinal.joined(separator: " | "))")
            }
        }

        // **The two axes, side by side, sorted so they cannot be read apart.**
        //
        // Sorted by the gap between them rather than by either column alone. That
        // ordering is a DISPLAY aid and carries no threshold: it exists because
        // reading the false-accept column on its own is what produced the question
        // this axis was added to answer, and a phrase that is best in that column
        // because it is deaf sinks to the bottom here without anyone having to do
        // the subtraction. Nothing in production reads this ordering, and no cut
        // is drawn anywhere in it — see `VoskWakeSpotter.assessment(of:)` for why
        // a continuum sampled at n=15 and n=\(speakers.count) does not get one.
        let ranked = scorecard.sorted { ($0.truePercent - $0.falsePercent) > ($1.truePercent - $1.falsePercent) }
        var paired: [String] = []
        for row in ranked {
            let bar = String(repeating: "█", count: row.truePercent / 10)
                + String(repeating: "·", count: 10 - row.truePercent / 10)
            let gap = row.truePercent - row.falsePercent
            paired.append(
                "│ " + Self.pad(row.phrase, 16)
                + "│ " + Self.pad("\(row.truePercent)%", 6) + bar
                + " │ " + Self.pad("\(row.falsePercent)%", 6)
                + "│ " + Self.pad("\(gap >= 0 ? "+" : "")\(gap)", 6) + "│"
            )
        }

        // The grid the phrase decision is made from: read DOWN a column for the
        // prefix effect, ACROSS a row for name strength.
        var grid: [String] = []
        for name in ["magician", "presto", "sam", "samy", "sammy", "sammie", "pico"] {
            let cells = [name, "hey \(name)", "listen \(name)"].map { phrase in
                Self.pad(finalRateByPhrase[phrase] ?? "—", 11)
            }
            grid.append("│ " + Self.pad(name, 10) + "│ " + cells.joined(separator: "│ ") + "│")
        }

        let voiceNames = SpokenAudio.englishVoices().map { "\($0.name)/\($0.language)" }
        print("""

        ┌── PHRASE SCORECARD: BOTH AXES ──────────────────────────────────────────────┐
        │ TRUE accepts = how many of \(speakers.count) synthetic renderings of the phrase WOKE it.
        │   \(SpokenAudio.englishVoices().count) voices x \(Self.prosodyGrid.count) prosody settings. Synthetic speaker variation, NOT
        │   human speech: a LOWER BOUND on the problem, not a real-world wake rate.
        │   Voices: \(voiceNames.joined(separator: ", "))
        │ FALSE accepts = how many of 15 phonetic near-misses ALSO woke it (lower is better).
        │ A phrase that never fires scores a perfect 0% false accepts. Read them together.
        ├─────────────────┬─────────────────┬────────┬───────┤
        │ phrase          │ TRUE accept ↑   │ false ↓│ gap   │
        ├─────────────────┼─────────────────┼────────┼───────┤
        \(paired.joined(separator: "\n"))
        └─────────────────────────────────────────────────────┘

        ┌── NAME x PREFIX GRID: near-miss FALSE ACCEPTS on FINALS (ships) ─────┐
        │ name      │ bare       │ hey X      │ listen X   │
        ├───────────┼────────────┼────────────┼────────────┤
        \(grid.joined(separator: "\n"))
        └──────────────────────────────────────────────────┘

        ┌── WAKE ACCURACY: NEAR-MISSES ────────────────────────────────────────────────────────────────┐
        │ phrase          │ PARTIAL     │ FINAL       │ fires@  │ fires@│ phrase  │
        │                 │ near-miss   │ near-miss   │ partial │ final │ length  │
        ├─────────────────┼─────────────┼─────────────┼─────────┼───────┼─────────┤
        \(accuracy.joined(separator: "\n"))
        └──────────────────────────────────────────────────────────────────────────┘

        ┌── WAKE ACCURACY: ADVERSARIAL SETS (all rates are FALSE accepts) ─────────────────────────────┐
        │ phrase          │ unrelated   │ OPENERS     │ OPENERS     │ prefix      │
        │                 │ (final)     │ (partial)   │ (FINAL)     │ confusions  │
        ├─────────────────┼─────────────┼─────────────┼─────────────┼─────────────┤
        \(adversarial.joined(separator: "\n"))
        └──────────────────────────────────────────────────────────────────────────┘
        \(detail.joined(separator: "\n"))

        """)
    }

    // MARK: - what finals actually cost

    /// **The question finals-only had to answer while the pre-roll was wired.**
    ///
    /// A finalised result arrives when the decoder decides the utterance ended,
    /// which needs silence. But the activation phrase and the request after it
    /// are ONE continuous utterance — the premise the since-unwired `WakePreRoll`
    /// existed for — so there is no silence after the phrase to finalise on.
    /// This measures where each mode fires in a realistic continuous sentence,
    /// and checked the result against what the 2 s pre-roll could cover. Since
    /// the 2026-07-30 decision removed pre-ready capture, the coverage column is
    /// historical context; the firing offsets themselves are still live numbers.
    func testFinalisationCostOnContinuousSpeech() async throws {
        let phrase = "hey magician"
        let sentences = [
            "hey magician what is the weather like tomorrow",
            "hey magician remind me to call the bank this afternoon",
            "hey magician play something quiet",
        ]
        let phraseMs = (await SpokenAudio.pcm16(phrase)).count / Self.bytesPerMillisecond
        var rows: [String] = []
        for sentence in sentences {
            let audio = await SpokenAudio.pcm16(sentence)
            guard !audio.isEmpty else { continue }
            let spokenMs = audio.count / Self.bytesPerMillisecond
            let partial = firingOffset(phrase: phrase, audio: audio, mode: .partial)
                .map { $0 / Self.bytesPerMillisecond }
            let final = firingOffset(phrase: phrase, audio: audio, mode: .final)
                .map { $0 / Self.bytesPerMillisecond }
            // The pre-roll was 2 s deep and held the newest audio, so at a hit
            // at time T it covered [T-2000, T]. The request starts when the
            // phrase ends, so the whole request survived iff T - 2000 <= phraseMs.
            let covered = final.map { $0 - 2_000 <= phraseMs }
            rows.append("""
            │ "\(sentence)"
            │   spoken: \(spokenMs) ms   phrase ends ~\(phraseMs) ms
            │   partial fires: \(partial.map { "\($0) ms" } ?? "never")
            │   final   fires: \(final.map { "\($0) ms" } ?? "never (no silence to finalise on)")
            │   delta:         \(partial.flatMap { p in final.map { "\($0 - p) ms later" } } ?? "n/a")
            │   pre-roll (2 s) still covers the request start: \(covered.map { $0 ? "YES" : "NO" } ?? "n/a — never fired")
            """)
        }
        print("""

        ┌── FINALS-ONLY ON CONTINUOUS SPEECH ──────────────────────────────────
        \(rows.joined(separator: "\n│\n"))
        └──────────────────────────────────────────────────────────────────────

        """)
    }
}
