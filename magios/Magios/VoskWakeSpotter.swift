import CVosk
import Foundation
import os

/// The on-device wake spotter: Vosk, constrained to a grammar of the activation
/// phrases plus `[unk]`.
///
/// **It has no networking collaborator, and that is the feature.** While armed,
/// the only things this type touches are a model directory inside the app bundle
/// and the PCM handed to `feed`. There is no session, no client, no upload — see
/// `WakeSpotter`. Adding one here would silently convert ambient mode from a
/// local gate into an open microphone, so the absence is load-bearing rather
/// than incidental.
///
/// ## Finalised results only
///
/// A hit is taken only from a finalised utterance, never from an in-progress
/// hypothesis, and there is no switch to change that. Measured, partials
/// false-accept on 73% of phonetically adjacent near-misses against 13% on
/// finals; the cost is ~1.2 s of visible latency. (Until 2026-07-30 the 2 s
/// `WakePreRoll` made that latency free of request-audio loss; the owner then
/// removed pre-ready capture entirely — nothing said before session-ready is
/// kept for anyone — so the latency now costs only responsiveness.) `feed`
/// carries the numbers.
///
/// ## Grammar-constrained, not open-vocabulary
///
/// The two existing Vosk integrations in this repo (`ui/.../wakeWord.ts` and
/// `desktop/.../voice_wake.rs`) transcribe open-vocabulary and then run
/// `text.lowercased().contains(phrase)`. That is the crudest filter available and
/// it is wrong in two independent ways: it decodes the entire English lexicon to
/// answer a yes/no question, and `contains` has no token boundary, so a phrase
/// like "sam" fires inside "Samantha". Neither is copied here. A grammar
/// restricts the decoder to the phrases plus `[unk]`, which is both far cheaper
/// and dramatically more accurate, and matching is done on whole tokens.
///
/// ## Why `[unk]` is mandatory
///
/// A grammar with no `[unk]` alternative has nowhere to put speech that is not a
/// phrase, so the decoder force-maps *every* utterance onto the nearest phrase —
/// measured false-accept rate approaches 100%. With `["<phrase>", "[unk]"]` the
/// decoder has somewhere to put everything else: measured 0 false accepts over
/// 8.5 minutes of unrelated speech. So `[unk]` is appended by
/// `grammarJSON(for:)` and is not a caller's option to omit.
///
/// ## Why re-grammaring destroys the recognizer
///
/// `vosk_recognizer_set_grm` exists on Android and is **absent from the iOS
/// binary** — it is not in the archive's symbol table and it is deliberately not
/// declared in the vendored header, so reaching for it does not compile rather
/// than failing to link. `configure` therefore frees the recognizer and builds a
/// new one with `vosk_recognizer_new_grm`. Measured under a millisecond, since
/// the expensive object is the model and that is kept.
///
/// ## Threading
///
/// `feed` runs on the audio thread; `configure` and `reset` run on the main
/// actor. This type carries **no lock**, and that is a property of the call
/// sites rather than of the type: `AmbientController` places every
/// `configure`/`reset` strictly before a `mic.start` or after a `mic.stop()`,
/// and `stop()` contractually guarantees no callback is still in flight. Moving
/// either inside a running tap breaks that silently. The one hop that *is* this
/// type's obligation is `onHit`, which is typed `@MainActor` precisely so
/// skipping it does not compile.
// `feed` is intentionally transferred to the audio callback queue while
// configuration remains on the main actor. AmbientController serializes those
// phases around mic start/stop, as documented above.
final class VoskWakeSpotter: WakeSpotter, @unchecked Sendable {

    /// Ignore repeat matches within this window.
    ///
    /// Still needed after the move to finalised results, for a narrower reason
    /// than before. Vosk endpoints on its own heuristics, so one continuous
    /// stretch of speech can finalise more than once and the phrase can land in
    /// two consecutive utterances; and the tail of a conversation re-triggers a
    /// spotter that was re-armed too eagerly (the lesson `8bd5c2f9e` records on
    /// the desktop side). The first is handled entirely here — the recognizer is
    /// also rebuilt on fire. The second is `AmbientController.resumeSpotting`'s to
    /// own, because only the controller knows a call just ended; the desktop's
    /// companion constant for it is `WAKE_RESUME_COOLDOWN_MS = 2500`.
    static let fireCooldown: TimeInterval = 4.0

    /// Vosk's own token for "speech that is not one of the phrases". Reserved,
    /// so it is never treated as a phrase word and never lexicon-checked.
    private static let unknownToken = "[unk]"

    var onHit: (@MainActor (WakeHit) -> Void)?

    /// What is known about a phrase — **not a verdict on it.**
    ///
    /// The first case is a refusal and the other three are reports. The split is
    /// not cosmetic: `vosk_model_find_word` is necessary but NOT sufficient, so a
    /// phrase can be perfectly in-lexicon and still be a poor gate, and the
    /// arming path needs different answers for "cannot arm" and "armed, and here
    /// is what the matrix says about it".
    ///
    /// **Three report cases rather than one, because "we measured this and it is
    /// 20%" and "nobody has ever measured this" are different facts and used to
    /// be the same silence.** The old shape had a single `tooWeak` case and
    /// returned nil for everything else, which meant an unmeasured phrase — every
    /// phrase built from a name nobody has tested, which is almost all of them —
    /// was reported identically to `hey magician`, the best row in the matrix.
    enum PhraseAssessment: Equatable {
        /// Not in the model's lexicon, so it cannot be armed at all.
        ///
        /// Vosk drops out-of-lexicon words from a grammar with nothing but a log
        /// warning, so "hey magican" — `vosk_model_find_word` returns -1 for "magican"
        /// — would arm as the bare phrase "hey" and fire on any sentence
        /// containing it. The silent failure of this engine is not a missed wake
        /// but a catastrophic false accept, so the phrase is refused whole.
        ///
        /// **The only refusing case.** The three below all arm.
        case notInLexicon(unknownWords: [String])

        /// This exact phrase went through the matrix, and here are **both** its
        /// numbers.
        ///
        /// **No verdict is attached, deliberately.** See `assessment(of:)` for
        /// why there is no threshold to attach one with.
        ///
        /// **Two associated rates rather than one, because one of them is
        /// unreadable alone.** A phrase that never fires scores a perfect 0% on
        /// the near-miss axis, so "best measured false-accept rate" and "deaf"
        /// produce the same cell — which is the reason the true-accept axis was
        /// added at all, and the reason the two arrive together here rather than
        /// through two lookups a caller could do one of.
        case measured(nearMissFalseAcceptPercent: Int, syntheticTrueAcceptPercent: Int, note: String)

        /// A single word, and not one the matrix has a number for.
        ///
        /// The one property of a phrase's *shape* that was measured as a class:
        /// every bare row was measured against its own prefixed form and none of
        /// them came out ahead.
        case bareWord(note: String)

        /// In the lexicon, armed, and **nobody has measured it.**
        ///
        /// This is the ordinary case in production — the phrase is built from the
        /// assistant's name and the matrix contains six names. It is reported
        /// rather than passed silently because "no measurement" and "a good
        /// measurement" are not the same claim, and the previous shape made them
        /// indistinguishable.
        case unmeasured(note: String)

        /// The user-facing sentence, or nil for the case that refuses (which is
        /// reported by `rejectedPhrases` and gets its own refusal message).
        var note: String? {
            switch self {
            case .notInLexicon: return nil
            case .measured(_, _, let note), .bareWord(let note), .unmeasured(let note): return note
            }
        }
    }

    /// Per-phrase assessments from the last `configure`, in the order supplied.
    private(set) var phraseAssessments: [(phrase: String, assessment: PhraseAssessment)] = []

    /// Phrases that could not be armed at all. **The arming path must consult
    /// this** — see `PhraseAssessment.notInLexicon`.
    var rejectedPhrases: [String] {
        phraseAssessments.compactMap { if case .notInLexicon = $0.assessment { return $0.phrase } else { return nil } }
    }

    /// What is known about each phrase that DID arm, in the order supplied.
    ///
    /// **Notes, not a warning flag.** This replaced a `weakPhrases: [String]`
    /// bucket, and the bucket had to go with the threshold that would have been
    /// needed to fill it: the measured rates run 6, 20, 33, 40, 46, 53, 60, 66,
    /// 80, 93 with no gap anywhere in them, so any membership test is a line through
    /// sampling noise. Carrying the sentence instead lets the surface show the
    /// number and the user do the comparing — which is the only comparison in
    /// this feature that has anything to compare against, since the phrase is
    /// derived from a name the user chose.
    var phraseNotes: [PhraseNote] {
        phraseAssessments.compactMap { entry in
            entry.assessment.note.map { PhraseNote(phrase: entry.phrase, note: $0) }
        }
    }

    /// Whether anything is actually being listened for. False after a
    /// `configure` whose phrases were all rejected — at which point the spotter
    /// can never fire, and an arming path that ignores this shows the user an orb
    /// over a microphone that will never wake.
    ///
    /// True is necessary but not sufficient for a *useful* window: every phrase
    /// in `phraseNotes` armed and set this, including the ones the matrix
    /// measured worst. Both have to be checked.
    var isArmed: Bool { recognizer != nil }

    /// Renderings behind every `syntheticTrueAcceptPercent` below: **ten**
    /// installed English voices at **five** prosody settings each.
    ///
    /// The ten, taken from the iOS 26.5 simulator image and enumerated because a
    /// rate over a thin voice set is only honest if the set is named beside it —
    /// Karen (en-AU), Daniel (en-GB), Moira (en-IE), Rishi (en-IN), Fred, Junior,
    /// Kathy, Ralph, Samantha (en-US), Tessa (en-ZA). All six accepted locales
    /// appear, but four of them by exactly one voice and en-US by five, so this
    /// is a thin set and an accent-poor one. The image ships 25 English voices;
    /// the other fifteen are novelty voices (Bells, Jester, Zarvox, Organ, Bad
    /// News, Albert...) excluded by Apple's `isNoveltyVoice` trait: they sing or
    /// robotise the text, and a wake rate dragged down by them would read as a
    /// fact about the phrase.
    ///
    /// **Recorded because a percentage over a thin, machine-specific voice set is
    /// exactly the kind of number that gets quoted later without its caveat.**
    /// `VoskWakeAccuracyMeasurementTests.testSyntheticSpeakerInventory` fails if
    /// the installed set stops producing this many, rather than letting the table
    /// keep percentages taken over a different denominator.
    static let syntheticSpeakerRenderings = 50

    /// Both measured axes for one phrase. **Neither is readable alone**, which is
    /// why they are one value rather than two tables that could drift apart.
    struct MeasuredRates: Equatable {
        /// Fraction of 15 deliberately phonetically-adjacent utterances that woke
        /// the phrase. Lower is better; a phrase that never fires scores 0.
        let nearMissFalseAcceptPercent: Int
        /// Fraction of `syntheticSpeakerRenderings` renderings **of the phrase
        /// itself** that woke it. Higher is better.
        ///
        /// **A lower bound on the problem, not a real-world wake rate.** Every
        /// rendering is one `AVSpeechSynthesizer` on one OS image: voices and
        /// prosody vary, but they share a pronunciation model and a studio-clean
        /// channel with no room, distance, background or disfluency in it. It
        /// catches a phrase the decoder is deaf to; it certifies nothing.
        let syntheticTrueAcceptPercent: Int
    }

    /// Both measured axes, by normalised phrase. **One table, so a future
    /// measurement run updates one place.**
    ///
    /// **The near-miss values are NEAR-MISS rates, not in-use rates, and reading
    /// them as in-use rates overstates the problem by a lot.** Each is the
    /// fraction of 15 deliberately phonetically-adjacent utterances that woke the
    /// phrase — the worst input it will ever see, constructed to be worst. On
    /// ordinary unrelated speech and on conversational openers, every prefixed
    /// `hey` row measured 0/8. All rows are finalised results (what ships), from
    /// `VoskWakeAccuracyMeasurementTests` under one uniform construction: 15
    /// phonetic neighbours of the *name*, each carrying the row's prefix.
    ///
    /// **n=15 is the reason nothing here is compared against a threshold.** One
    /// case is about seven points, so 46% and 53% are 7/15 and 8/15 — adjacent
    /// rows are one utterance apart. Every value is n/15 by construction, and
    /// `testEveryTabledRateIsAFifteenth` fails a row that is not.
    ///
    /// **`hey sammy` and `hey sammie` are known to move by one case between runs**
    /// (6/15 ↔ 7/15) and are recorded at their upper value; see
    /// `assessment(of:)`. Nothing else in the near-miss column has ever moved.
    ///
    /// The `hey magician` row is 20% here and `VoskWakeSpotterTests` measures 13%
    /// for the same phrase in the same mode; both are right and neither should
    /// replace the other. That suite uses its own original fifteen candidates and
    /// is what the regression guard is pinned to; this table uses the uniform
    /// construction, which is what makes the rows comparable *with each other*.
    ///
    /// **The true-accept column answers the question the near-miss column cannot
    /// be asked.** Read alone, the near-miss column ranks a phrase that never
    /// fires first. Every row here is 88% or better, so **no phrase in this matrix
    /// is deaf** — including `hey samy`, whose 6% was suspected of being a
    /// measurement of silence and is not.
    /// **`magican` is not in this model's vocabulary (probe, 2026-09-01).**
    /// Compiling `["hey magican", "[unk]"]` logs *"Ignoring word missing in
    /// vocabulary: 'magican'"* and yields a 3-state/4-arc grammar — byte-for-byte
    /// the shape produced by a nonsense control word, where every in-vocabulary
    /// phrase yields 4 states and 7 arcs. It matched 0/15 clips including its own.
    /// That state/arc count is a cheap offline test for any future name.
    ///
    /// The agent definition's `wake_spellings` exists for exactly this: Vosk
    /// hears "magican" as **magical** (3/3 across three synthetic voices; only
    /// 1/3 as "magician"), so `hey magical` is armed in its place.
    ///
    /// Provenance differs from the rows below and the numbers are NOT
    /// interchangeable with them: 3 macOS `say` voices for true-accept, and a
    /// 45-utterance near-miss corpus of ordinary speech for false-accept —
    /// not the protocol behind this table. Directionally, `hey magical` scored
    /// 100% true-accept / 0% false-accept, while **bare `magical` scored 33%
    /// false-accept**, firing on all five corpus sentences containing the word.
    /// That independently reproduces this table's own lesson: the prefix is what
    /// makes a common word safe, and bare rows are the ones not to pick.
    static let measured: [String: MeasuredRates] = [
        "magician": .init(nearMissFalseAcceptPercent: 33, syntheticTrueAcceptPercent: 100),
        "hey magician": .init(nearMissFalseAcceptPercent: 20, syntheticTrueAcceptPercent: 92),
        "listen magician": .init(nearMissFalseAcceptPercent: 20, syntheticTrueAcceptPercent: 98),
        "presto": .init(nearMissFalseAcceptPercent: 33, syntheticTrueAcceptPercent: 100),
        "hey presto": .init(nearMissFalseAcceptPercent: 33, syntheticTrueAcceptPercent: 88),
        "listen presto": .init(nearMissFalseAcceptPercent: 60, syntheticTrueAcceptPercent: 100),
        "sam": .init(nearMissFalseAcceptPercent: 80, syntheticTrueAcceptPercent: 100),
        "hey sam": .init(nearMissFalseAcceptPercent: 53, syntheticTrueAcceptPercent: 96),
        "listen sam": .init(nearMissFalseAcceptPercent: 93, syntheticTrueAcceptPercent: 100),
        "samy": .init(nearMissFalseAcceptPercent: 33, syntheticTrueAcceptPercent: 100),
        "hey samy": .init(nearMissFalseAcceptPercent: 6, syntheticTrueAcceptPercent: 94),
        "sammy": .init(nearMissFalseAcceptPercent: 66, syntheticTrueAcceptPercent: 100),
        "hey sammy": .init(nearMissFalseAcceptPercent: 46, syntheticTrueAcceptPercent: 94),
        "sammie": .init(nearMissFalseAcceptPercent: 66, syntheticTrueAcceptPercent: 100),
        "hey sammie": .init(nearMissFalseAcceptPercent: 46, syntheticTrueAcceptPercent: 94),
        // **Bare `pico` has the best near-miss number of any bare row ever
        // measured and is still the row not to pick.** It is the only phrase in
        // the matrix that false-accepts on ordinary unrelated speech on finals
        // (1/8 — "i need to pick up groceries on the way home"), which no column
        // in this table shows. `hey pico` is 0/8 there.
        "pico": .init(nearMissFalseAcceptPercent: 13, syntheticTrueAcceptPercent: 98),
        "hey pico": .init(nearMissFalseAcceptPercent: 20, syntheticTrueAcceptPercent: 92),
    ]

    /// The near-miss axis on its own, derived so it cannot drift from `measured`.
    ///
    /// **Kept as a separate name because reading it alone is a mistake with a
    /// history**, not because it is a separate fact: every wake-phrase decision
    /// through 2026-07-29 rested on this column, and `hey samy`'s 6% — the best
    /// value in it — turned out to be worth re-reading next to the other axis.
    static let measuredNearMissFalseAcceptPercent: [String: Int] =
        measured.mapValues(\.nearMissFalseAcceptPercent)

    /// What the matrix says about a phrase, always — there is no "nothing to
    /// report".
    ///
    /// ## The rule that was removed, and why nothing replaced it
    ///
    /// This carried two rules and **only one of them had ever been measured.**
    /// The retired one flagged a phrase whose longest word was three letters or
    /// fewer, generalising from five rows to every phrase that can be typed on
    /// the reasoning that `hey sam` was weak *because* `sam` was short. The first
    /// run to sample where that could be checked falsified it. Longest word
    /// against measured rate:
    ///
    /// | phrase | longest word | near-miss false accepts |
    /// | --- | --- | --- |
    /// | `hey samy` | 4 | 1/15 (6%) |
    /// | `hey magician` | 8 | 3/15 (20%) |
    /// | `hey presto` | 6 | 5/15 (33%) |
    /// | `hey sammy` | 5 | 7/15 (46%) |
    /// | `hey sammie` | 6 | 7/15 (46%) |
    /// | `hey sam` | 3 | 8/15 (53%) |
    ///
    /// Letter count is not monotone against the rate, and **no cut on it
    /// separates these rows.** `<= 5` catches `hey sam` (53%) and `hey sammy`
    /// (46%), the two worst — and also catches `hey samy` (6%), the best row ever
    /// measured. `<= 4` catches `hey sam` and `hey samy` and nothing else: the
    /// worst row and the best row together, with neither of the two in between
    /// them. The shipped rule's own verdict on `hey sammy` was *pass*, at 46%,
    /// one utterance away from the `hey sam` it flagged.
    ///
    /// **The three `sam*` rows share byte-identical audio, and that is the whole
    /// argument in one line.** `samy`, `sammy` and `sammie` are measured against
    /// the same fifteen rendered clips and the synthesiser gives the phrases
    /// themselves the same duration to the millisecond; the only thing that
    /// differs between the rows is which lexicon entry the spelling resolves to
    /// (75756, 73221, 73220). Same sound in, 6% against 46% out. Whatever a string
    /// test is reading, it is not what the decoder is doing.
    ///
    /// **No numeric threshold replaces the retired rule, and that is a refusal
    /// rather than an omission.** The rates form a continuum — 6, 20, 20, 33,
    /// 33, 33, 33, 46, 46, 53, 60, 66, 66, 80, 93 — with no gap in it, at n=15
    /// where a single case is seven points.
    ///
    /// **That noise floor was observed, not assumed, and it landed exactly where
    /// a threshold would have.** The matrix was run four times while this was
    /// being written — same harness, same build, same machine, same simulator.
    /// `hey sammy` came back 7/15 three times and 6/15 once; `hey sammie` 7/15
    /// twice and 6/15 twice. **Those two cells are the only ones in the whole
    /// finals column that ever moved**; the other thirteen were identical in
    /// every run. They are also the two rows a cut between `hey presto` (33%) and
    /// `hey sam` (53%) would have been drawn beside — so that cut would classify
    /// them differently run to run with nothing about the phrases changing.
    ///
    /// The table below therefore records 46% for both, which is `hey sammy`'s
    /// modal value and the reading that treats two rows built from the *same
    /// audio* as indistinguishable at this n. **A future run that comes back 40%
    /// on either has not found a regression** — it has re-observed this. Change
    /// the table when a row moves by more than one case, not when it moves by one.
    ///
    /// So this function carries the number and leaves the comparison to the
    /// surface showing it: `SettingsView` before arming, `AmbientMiniBar` during
    /// the window.
    ///
    /// ## The rule that survives, and the row that dented it
    ///
    /// **Word count**, because it was measured as a class rather than inferred
    /// from one row. Seven bare names have now been measured against their own
    /// prefixed form: 33→20 on `magician`, 33→33 on `presto`, 80→53 on `sam`,
    /// 33→6 on `samy`, 66→46 on `sammy`, 66→46 on `sammie` — and **13→20 on
    /// `pico`**, the first row where the bare form came out ahead. Five
    /// improvements, one tie, one reversal of a single utterance at n=15, which
    /// is this matrix's stated noise floor.
    ///
    /// The rule is kept and the exception is stated in the note rather than
    /// rounded away, because the alternative is a class claim with a known
    /// counterexample hidden inside it. Two things narrow the exception without
    /// erasing it: it is 2/15 against 3/15, one case; and **bare `pico` is the
    /// only phrase in the whole matrix that false-accepts on ordinary unrelated
    /// speech on finals** (1/8, "i need to pick up groceries on the way home"),
    /// where `hey pico` is 0/8. The bare form wins the near-miss column and loses
    /// the column that is actually held to zero.
    ///
    /// ## No threshold on the second axis either
    ///
    /// The true-accept column runs 88, 92, 92, 94, 94, 94, 96, 98, 98, 100 ×7 —
    /// a twelve-point band at n=50, where one rendering is two points, so the
    /// worst row is six renderings from the best. Across two runs of the harness
    /// on one build and one machine the column did not move at all. That is a
    /// tighter column than the near-miss one and it still has no gap in it: any
    /// cut would sit inside a six-case spread and would separate `hey presto`
    /// (88%) from `hey magician` (92%) on two renderings, both of which are
    /// perfectly usable phrases. **The number is carried; no verdict is derived
    /// from it**, for the same reason the near-miss column has none.
    ///
    /// What the column IS for: it makes "best false-accept rate" unreadable as
    /// "best phrase", and it retires one specific suspicion — that a very low
    /// near-miss rate might be deafness rather than discrimination. It is not:
    /// `hey samy` is 6% and wakes on 47 of 50 renderings.
    ///
    /// ## What this is not
    ///
    /// It does not refuse anything. It does not rank phrases, recommend one, or
    /// claim any phrase is good: the floor is set by the engine, not by the
    /// phrase. `hey samy`, the best row anywhere in the matrix, still wakes on 1
    /// of 15 phrases that merely sound like it and on 4 of 5 manglings of its own
    /// prefix — best in one column is not good, and not best in another.
    ///
    /// And the true-accept number is **not** a wake rate. Every rendering behind
    /// it is one synthesiser on a clean channel; a person in a room is a harder
    /// input along axes this cannot reach. It bounds the problem from below.
    static func assessment(of phrase: String) -> PhraseAssessment {
        let normalised = normalise(phrase)
        let words = normalised.split(separator: " ").map(String.init)
        // Nothing survived normalisation, so there is no phrase to say anything
        // about — the same answer `configure` gives it, and for the same reason:
        // an empty grammar entry is not armable.
        guard !words.isEmpty else { return .notInLexicon(unknownWords: []) }
        if let rates = measured[normalised] {
            return .measured(
                nearMissFalseAcceptPercent: rates.nearMissFalseAcceptPercent,
                syntheticTrueAcceptPercent: rates.syntheticTrueAcceptPercent,
                // **Both numbers, in this order, in one sentence.** The wake rate
                // first because it is the one the user asked for by choosing a
                // name, and because the false-accept number alone was readable as
                // a compliment on a phrase that never fires.
                note: "Measured: it woke on \(rates.syntheticTrueAcceptPercent)% of "
                    + "\(syntheticSpeakerRenderings) synthetic voice renderings of itself, and "
                    + "\(rates.nearMissFalseAcceptPercent)% of 15 deliberately similar-sounding "
                    + "phrases also woke it."
                    + (words.count == 1
                        ? " It is also a single word, and no single word measured better than the same name with “Hey” in front of it."
                        : "")
            )
        }
        if words.count == 1 {
            return .bareWord(
                note: """
                A single word on its own. The seven that were measured woke on 98% to 100% of \
                synthetic voice renderings — a shade more reliably than any phrase with “Hey” in \
                front — and on 13% to 80% of similar-sounding phrases, which is far worse. Adding \
                “Hey” improved five of the seven, tied one, and cost one a single utterance.
                """
            )
        }
        return .unmeasured(
            note: """
            This phrase has never been measured. The ones that were run woke on 88% to 100% of \
            synthetic voice renderings of themselves, and on 6% to 93% of similar-sounding phrases — \
            so there is no telling yet which end of that second range this is.
            """
        )
    }

    private let modelURL: URL?
    private let sampleRate: Float
    private let log = Logger(subsystem: "ai.magicbeans.magican", category: "wake")

    private var model: OpaquePointer?
    private var recognizer: OpaquePointer?
    private var modelLoadFailed = false
    /// The grammar the current recognizer was built from, kept so `reset()` can
    /// rebuild rather than clear. See `rebuildRecognizer`.
    private var grammar: String?

    /// Normalised phrase → the phrase exactly as the caller supplied it, so a
    /// `WakeHit` reports what the user configured rather than our lowercasing.
    private var phrasesByNormalised: [String: String] = [:]
    private var lastFire: Date?

    /// - Parameter modelURL: the unpacked model directory. Defaults to the copy
    ///   bundled in the app (see the doc for why it is bundled rather than an
    ///   On-Demand Resource). Injectable so tests can point at a fixture.
    init(modelURL: URL? = VoskWakeSpotter.bundledModelURL, sampleRate: Float = 16_000) {
        self.modelURL = modelURL
        self.sampleRate = sampleRate
        // Kaldi is extremely chatty on stderr and says nothing actionable.
        vosk_set_log_level(-1)
    }

    deinit {
        vosk_recognizer_free(recognizer)
        vosk_model_free(model)
    }

    /// The model folder reference copied into the app bundle by `project.yml`.
    static var bundledModelURL: URL? {
        Bundle.main.url(forResource: "vosk-model", withExtension: nil)
    }

    // MARK: - WakeSpotter

    func configure(phrases: [String]) {
        // Replace, never append — the protocol says so, and an additive
        // implementation would leave a previous window's phrase armed into the
        // next one.
        phrasesByNormalised = [:]
        phraseAssessments = []
        vosk_recognizer_free(recognizer)
        recognizer = nil
        grammar = nil
        lastFire = nil

        guard let model = loadModelIfNeeded() else {
            phraseAssessments = phrases.map { ($0, .notInLexicon(unknownWords: [])) }
            return
        }

        for phrase in phrases {
            let normalised = Self.normalise(phrase)
            guard !normalised.isEmpty else {
                phraseAssessments.append((phrase, .notInLexicon(unknownWords: [])))
                continue
            }
            let missing = normalised.split(separator: " ")
                .map(String.init)
                .filter { vosk_model_find_word(model, $0) < 0 }
            guard missing.isEmpty else {
                // `.fault` rather than `.error`: this is a misconfiguration that
                // disables the feature, and it is invisible everywhere else.
                log.fault(
                    """
                    wake phrase rejected — not in the model lexicon: \
                    \(phrase, privacy: .public) (unknown: \(missing.joined(separator: ", "), privacy: .public))
                    """
                )
                phraseAssessments.append((phrase, .notInLexicon(unknownWords: missing)))
                continue
            }
            // In-lexicon, so it arms — and something is recorded about EVERY
            // phrase that does. Recording only the bad ones is what the previous
            // shape did, and it made "measured and fine" and "never measured"
            // the same silence.
            //
            // `.notice`, not `.fault`: this is no longer a warning about a
            // failure, it is what is known about the phrase. The user reads the
            // assessment pre-arm, via `assessment(of:)` in Settings; post-arm
            // the number lives in `phraseNotes` and this log line only.
            let assessment = Self.assessment(of: phrase)
            if let note = assessment.note {
                log.notice("wake phrase armed: \(phrase, privacy: .public) — \(note, privacy: .public)")
            }
            phraseAssessments.append((phrase, assessment))
            phrasesByNormalised[normalised] = phrase
        }

        guard !phrasesByNormalised.isEmpty else {
            log.fault("wake spotter armed with no usable phrases — it can never fire")
            return
        }
        grammar = Self.grammarJSON(for: Array(phrasesByNormalised.keys))
        rebuildRecognizer()
        if recognizer == nil {
            log.fault("vosk_recognizer_new_grm returned NULL — wake spotting is inert")
        }
    }

    func feed(_ pcm: Data) {
        guard let recognizer, !pcm.isEmpty else { return }
        let count = pcm.count / MemoryLayout<Int16>.size
        guard count > 0 else { return }
        // Copied rather than pointer-cast: `Data`'s storage carries no alignment
        // guarantee, and binding misaligned memory to `Int16` is undefined. The
        // frames are a few KB, so the copy is far cheaper than the decode that
        // follows it.
        let samples = [Int16](unsafeUninitializedCapacity: count) { buffer, initialised in
            _ = pcm.copyBytes(to: UnsafeMutableRawBufferPointer(buffer), count: count * MemoryLayout<Int16>.size)
            initialised = count
        }
        let finished = samples.withUnsafeBufferPointer {
            vosk_recognizer_accept_waveform_s(recognizer, $0.baseAddress, Int32(count))
        }
        // FINALISED RESULTS ONLY. Partials are deliberately not in this path,
        // and there is no flag to put them back — a mode switch here is a
        // footgun, because the wrong setting is silent and costs a false wake.
        //
        // Measured, matching on partials false-accepts on 73% of phonetically
        // adjacent near-misses versus 13% on finals for "hey magician" (see
        // `VoskWakeAccuracyMeasurementTests`). The mechanism: the grammar's only
        // path beginning with the first word ends in the last one, so once the
        // opening syllables land the decoder's best in-progress hypothesis is
        // already the whole phrase — it fires before the discriminating word has
        // been spoken at all.
        //
        // The obvious objection is latency, and it does not survive measurement.
        // Vosk endpoints on its own heuristics rather than on true end of speech,
        // so it finalises mid-sentence: a flat +1200 ms across requests running
        // 1.9-3.0 s, landing at ~1600 ms. Until 2026-07-30 the 2 s `WakePreRoll`
        // recovered [T-2000, T] at the handoff, so the delay cost no request
        // audio; the owner then removed pre-ready capture entirely — nothing
        // said before session-ready is kept — so the delay now costs only the
        // same 1.2 s of visible responsiveness, no longer offset by a replay,
        // and the old coupling between pre-roll length and this decision is
        // severed. The accuracy numbers above are what the decision stands on.
        guard finished == 1 else { return }
        guard let heard = Self.text(fromResultJSON: vosk_recognizer_result(recognizer)),
              !heard.isEmpty
        else { return }
        guard let phrase = matchedPhrase(in: heard) else { return }
        fire(phrase)
    }

    /// Drop everything the decoder is holding.
    ///
    /// **This rebuilds the recognizer rather than calling
    /// `vosk_recognizer_reset`, because on this binary `vosk_recognizer_reset`
    /// does not discard an utterance still in progress.** Measured: feed 40% of
    /// the phrase, reset, then feed nothing but silence — the pending partial
    /// finalises and fires, on audio from before the reset. That is precisely
    /// the failure `reset()` exists to prevent, and it is the one that lets a
    /// previous armed stretch wake the next one.
    ///
    /// Rebuilding is affordable at exactly the rate this is called — once per
    /// arm and once per resume, never per frame — and the design measured
    /// recognizer construction at under a millisecond, which is why re-grammaring
    /// works this way too.
    func reset() {
        rebuildRecognizer()
        // Cleared together with the decoder state: `reset()` means the next armed
        // stretch starts from nothing, and a surviving `lastFire` would suppress
        // its first legitimate wake for up to the cooldown.
        lastFire = nil
    }

    /// Free the current recognizer and build a fresh one from the stored grammar.
    /// A no-op when nothing is armed, so a `reset()` on a spotter whose phrases
    /// were all rejected is harmless — an ordinary path, since
    /// `AmbientController.arm` calls `configure` then `reset` unconditionally.
    private func rebuildRecognizer() {
        guard let model, let grammar else { return }
        vosk_recognizer_free(recognizer)
        recognizer = vosk_recognizer_new_grm(model, sampleRate, grammar)
        if recognizer == nil { log.fault("vosk_recognizer_new_grm returned NULL on rebuild") }
    }

    // MARK: - matching

    /// The configured phrase present in `heard` as a contiguous run of WHOLE
    /// tokens, or nil.
    ///
    /// Token-wise rather than `String.contains`, which is the bug in both
    /// existing integrations: a substring test matches "sam" inside "Samantha",
    /// and with a wake word the cost of that is an open microphone.
    private func matchedPhrase(in heard: String) -> String? {
        let tokens = heard.split(separator: " ").map(String.init)
        guard !tokens.isEmpty else { return nil }
        for (normalised, original) in phrasesByNormalised {
            let needle = normalised.split(separator: " ").map(String.init)
            guard !needle.isEmpty, needle.count <= tokens.count else { continue }
            for start in 0...(tokens.count - needle.count)
            where Array(tokens[start..<(start + needle.count)]) == needle {
                return original
            }
        }
        return nil
    }

    private func fire(_ phrase: String) {
        guard recognizer != nil else { return }
        let now = Date()
        if let lastFire, now.timeIntervalSince(lastFire) < Self.fireCooldown { return }
        lastFire = now
        // Rebuild rather than `vosk_recognizer_reset`, for the reason `reset()`
        // gives: clearing leaves the in-progress utterance behind, so the matched
        // partial would grow by another word and match again, or finalise into a
        // second hit the moment the user stopped speaking.
        rebuildRecognizer()
        let hit = WakeHit(phrase: phrase, at: now)
        // The hop `WakeSpotter.onHit` exists to force. `onHit` is read *inside*
        // the main-actor task rather than captured out here, because it is
        // written on the main actor and reading it from the audio thread would
        // race the one property the controller assigns.
        Task { @MainActor [weak self] in self?.onHit?(hit) }
    }

    // MARK: - model

    private func loadModelIfNeeded() -> OpaquePointer? {
        if let model { return model }
        guard !modelLoadFailed else { return nil }
        guard let modelURL else {
            log.fault("no Vosk model bundled — run scripts/setup-magios-vosk.sh and regenerate")
            modelLoadFailed = true
            return nil
        }
        // `vosk_model_new` takes a filesystem path, so `path` rather than an
        // encoded URL string.
        guard let loaded = vosk_model_new(modelURL.path) else {
            log.fault("failed to load Vosk model at \(modelURL.path, privacy: .public)")
            modelLoadFailed = true
            return nil
        }
        model = loaded
        return loaded
    }

    // MARK: - pure helpers

    /// Lowercased, punctuation-stripped, single-spaced — the shape the Vosk
    /// lexicon stores words in, so it is also the shape `vosk_model_find_word`
    /// has to be asked about.
    static func normalise(_ phrase: String) -> String {
        phrase
            .lowercased()
            .map { $0.isLetter || $0.isNumber || $0 == "'" ? $0 : " " }
            .reduce(into: "") { $0.append($1) }
            .split(separator: " ")
            .joined(separator: " ")
    }

    /// The JSON array Vosk expects, with `[unk]` always appended.
    ///
    /// Sorted so the grammar is a deterministic function of the phrase set,
    /// which is what makes `configure` assertable at all — `phrasesByNormalised`
    /// is a dictionary and its key order is not stable across runs.
    static func grammarJSON(for normalisedPhrases: [String]) -> String {
        let quoted = (normalisedPhrases.sorted() + [unknownToken])
            .map { "\"\($0.replacingOccurrences(of: "\"", with: ""))\"" }
        return "[\(quoted.joined(separator: ","))]"
    }

    /// Pull `text` (finals) or `partial` (in-progress) out of a Vosk result.
    ///
    /// Hand-parsed via `JSONSerialization` rather than `Decodable` because the
    /// two shapes differ by key and both are single-field; a `Codable` pair would
    /// be more code to say the same thing.
    static func text(fromResultJSON json: UnsafePointer<CChar>?) -> String? {
        guard let json else { return nil }
        let raw = String(cString: json)
        guard let data = raw.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        let value = (object["text"] as? String) ?? (object["partial"] as? String) ?? ""
        // Vosk renders unmatched audio as the literal `[unk]`; it is not a word
        // anyone configured, so strip it before matching rather than letting it
        // sit between two halves of a phrase.
        return value
            .split(separator: " ")
            .filter { $0 != Substring(unknownToken) }
            .joined(separator: " ")
    }
}
