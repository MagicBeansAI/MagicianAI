import SwiftUI
import XCTest
@testable import Magician

/// The orb's phase → appearance mapping, which is the only part of the Live
/// Activity that can be proven at all.
///
/// A Live Activity view is archived SwiftUI rendered out-of-process by
/// WidgetKit, so a snapshot test of one asserts the harness rather than the
/// product. The mapping is where the decisions live, and it was factored into
/// `Shared/AmbientOrbAppearance.swift` for exactly that reason: a pure function
/// over value types, with no WidgetKit, no Dynamic Island and no device.
final class AmbientOrbAppearanceTests: XCTestCase {

    /// The single most important reading on this surface. `armed` means a
    /// microphone is open and nobody is talking to it, and it has to look like
    /// ambient presence rather than active recording — otherwise the user learns
    /// to ignore an indicator that always looks the same.
    ///
    /// Deliberately does NOT pin the fill's value. The resting fill is the one
    /// most likely to need retuning once someone sees an 8pt dot on a real lock
    /// screen, and a test naming the colour would turn that one-line change into
    /// two. What it must not be — a conversing colour, or invisible under Reduce
    /// Motion — is already proven below, over every case rather than this one.
    func testArmedReadsAsAmbientPresenceRatherThanActiveRecording() {
        let armed = AmbientOrbPhase.armed.appearance(agentName: "Sam")

        XCTAssertFalse(armed.isConversing, "the compact trailing slot stays empty while merely armed")
        XCTAssertLessThan(armed.scale, 1, "armed is the smallest orb, not the largest")
    }

    /// Everything that is not `armed` is a live exchange, and the trailing slot
    /// appearing is what tells the user that from a glance at the Dynamic Island.
    func testEveryConversationPhaseReadsAsConversing() {
        for phase in AmbientOrbPhase.allCases where phase != .armed {
            XCTAssertTrue(phase.appearance(agentName: "Sam").isConversing, "\(phase) is a live exchange")
        }
    }

    /// Colour is the primary carrier because it is the only channel that
    /// survives the minimal presentation — one dot, no text, too little area for
    /// scale to read. So two phases sharing a fill are one state to the user,
    /// and there is exactly one sanctioned exception: `heard` settling into
    /// `listening` (pinned separately below).
    ///
    /// Quantified over `allCases` rather than a hand-written list, so a sixth
    /// phase has to justify its colour instead of quietly inheriting one.
    func testNoTwoPhasesShareAColourExceptTheSanctionedOne() {
        let phases = AmbientOrbPhase.allCases
        for (index, phase) in phases.enumerated() {
            for other in phases[(index + 1)...]
            where phase.appearance(agentName: "Sam").fill == other.appearance(agentName: "Sam").fill {
                XCTAssertEqual(
                    Set([phase, other]),
                    Set([.heard, .listening]),
                    "\(phase) and \(other) render the same colour, which makes them one state to the user"
                )
            }
        }
    }

    /// The wake instant is the one moment the user needs confirmed, and out here
    /// the transition IS the animation — there is no render loop, so the jump
    /// between two content states is all the motion available. Dim white to full
    /// purple, at the widest scale in the set, is that jump.
    func testHeardIsTheLargestJumpFromArmed() {
        let armed = AmbientOrbPhase.armed.appearance(agentName: "Sam")
        let heard = AmbientOrbPhase.heard.appearance(agentName: "Sam")

        XCTAssertNotEqual(heard.fill, armed.fill)
        XCTAssertEqual(heard.scale, AmbientOrbPhase.allCases.map { $0.appearance(agentName: "Sam").scale }.max() ?? 0)
    }

    /// `heard` settles into `listening` without changing colour: the wake was
    /// the event, this is the steady state it settles into. Pinned because it is
    /// the one place the mapping deliberately reuses a colour, and a future
    /// reader would otherwise read it as an oversight.
    func testListeningSettlesOutOfHeardWithoutChangingColour() {
        let heard = AmbientOrbPhase.heard.appearance(agentName: "Sam")
        let listening = AmbientOrbPhase.listening.appearance(agentName: "Sam")

        XCTAssertEqual(heard.fill, listening.fill)
        XCTAssertLessThan(listening.scale, heard.scale)
    }

    /// `heard` also renders during an explicit system tap, so its public copy
    /// must describe the shared connect without claiming speech was detected.
    func testConnectingCopyIsNeutralBetweenWakeAndExplicitTalk() {
        let connecting = AmbientOrbPhase.heard.appearance(agentName: "Sam")

        XCTAssertEqual(connecting.status, "Starting conversation…")
        XCTAssertEqual(connecting.compactWord, "Starting…")
        XCTAssertFalse(connecting.status.localizedCaseInsensitiveContains("heard"))
    }

    func testEveryPhaseHasItsOwnStatusWord() {
        let statuses = AmbientOrbPhase.allCases.map { $0.appearance(agentName: "Sam").status }
        XCTAssertEqual(Set(statuses).count, AmbientOrbPhase.allCases.count, "got \(statuses)")
        XCTAssertFalse(statuses.contains(where: \.isEmpty), "a blank status line proves nothing to the user")
    }

    /// The orb says the agent's name rather than a build constant — the reason
    /// `AmbientActivityAttributes.agentName` is an attribute at all.
    func testTheArmedStatusNamesTheAgent() {
        XCTAssertEqual(AmbientOrbPhase.armed.appearance(agentName: "Sam").status, "Available — say Hey Sam")
    }

    /// The roster is scope-dependent and the lookup can come back empty, which
    /// would otherwise render a dangling wake instruction on the one line that
    /// tells the user their ambient window is open.
    func testABlankAgentNameDegradesToAPlainStatusRatherThanADanglingOne() {
        for name in ["", "   "] {
            XCTAssertEqual(AmbientOrbPhase.armed.appearance(agentName: name).status, "Available")
        }
    }

    /// An ended window must not keep claiming to be heard. This is the same lie
    /// as an armed microphone with no orb, inverted — and it is reachable,
    /// because an ended activity keeps rendering its final content for the whole
    /// dismissal window, and a process killed past its cap never publishes an
    /// end at all (`context.isStale` covers that one).
    func testAnEndedOrbShowsTheReasonAndNeverClaimsToBeListening() {
        let ended = AmbientOrbAppearance.ended(reason: AmbientEndedReason.capReached)

        XCTAssertEqual(ended.status, AmbientEndedReason.capReached)
        XCTAssertFalse(ended.isConversing)
        for phase in AmbientOrbPhase.allCases {
            XCTAssertNotEqual(ended.fill, phase.appearance(agentName: "Sam").fill, "\(phase) must not read as ended")
            XCTAssertNotEqual(ended.status, phase.appearance(agentName: "Sam").status)
        }
    }

    // MARK: - one event, one sentence

    /// **The equality that matters.** The cap firing and `context.isStale` are
    /// one event seen from two processes: the app publishes a reason when its
    /// cap timer fires, and the widget reconstructs one when no process was
    /// alive to publish anything (every publish carries `staleDate: expiresAt`,
    /// so stale means past the cap). The user must read the same sentence
    /// whichever side reaches them, and the two sites live in different modules
    /// with nothing structural keeping them equal — until they became one value.
    ///
    /// Asserted against the constant rather than against the literal text, so
    /// rewording the sentence is one edit and reintroducing a *second* sentence
    /// is a failure.
    func testTheStaleFallbackIsTheSentenceTheCapPublishes() {
        let reconstructedByTheWidget = AmbientOrbAppearance.forWindow(
            phase: .listening,
            endedReason: nil,
            isStale: true,
            agentName: "Sam"
        )
        let publishedByTheApp = AmbientOrbAppearance.ended(reason: AmbientEndedReason.capReached)

        XCTAssertEqual(reconstructedByTheWidget.status, publishedByTheApp.status)
        XCTAssertEqual(reconstructedByTheWidget, publishedByTheApp, "one event must render one way, not merely say one thing")
    }

    /// The distinction the shared constants exist to preserve, not erase.
    ///
    /// `capReached` asserts a cause — the window ran its full course, which the
    /// app knows because its own timer fired. The orphan sweep knows no such
    /// thing: it runs after the fact over an orb whose process is gone, and
    /// wording it as the cap would claim a cause nobody observed. Collapsing
    /// these into one string is the tempting cleanup, and it is why they are
    /// pinned apart here rather than left to a comment.
    func testTheOrphanSweepDoesNotClaimTheCapFired() {
        XCTAssertNotEqual(AmbientEndedReason.orphanCollected, AmbientEndedReason.capReached)
        XCTAssertFalse(AmbientEndedReason.orphanCollected.isEmpty)
        XCTAssertFalse(AmbientEndedReason.capReached.isEmpty)
    }

    /// `forWindow` and `windowIsOver` read the same two inputs and must agree.
    /// If they drift, the orb renders a live status word with its leash timer
    /// hidden, or an ended one with a countdown still ticking under it — either
    /// way a window whose two halves describe different states.
    func testWhateverIsOverRendersAsEndedAndWhateverIsNotRendersItsPhase() {
        let overCases: [(String?, Bool)] = [
            (AmbientEndedReason.capReached, false),
            (AmbientEndedReason.orphanCollected, false),
            (nil, true),
            (AmbientEndedReason.capReached, true)
        ]
        for phase in AmbientOrbPhase.allCases {
            for (reason, stale) in overCases {
                XCTAssertTrue(AmbientOrbAppearance.windowIsOver(endedReason: reason, isStale: stale))
                let appearance = AmbientOrbAppearance.forWindow(
                    phase: phase, endedReason: reason, isStale: stale, agentName: "Sam"
                )
                XCTAssertFalse(appearance.isConversing, "\(phase) over(\(reason ?? "nil"), \(stale)) still reads as live")
                XCTAssertNotEqual(
                    appearance.status,
                    phase.appearance(agentName: "Sam").status,
                    "an ended window must not keep showing its last phase's word"
                )
            }

            XCTAssertFalse(AmbientOrbAppearance.windowIsOver(endedReason: nil, isStale: false))
            XCTAssertEqual(
                AmbientOrbAppearance.forWindow(phase: phase, endedReason: nil, isStale: false, agentName: "Sam"),
                phase.appearance(agentName: "Sam"),
                "a live window renders its phase and nothing else"
            )
        }
    }

    // MARK: - motion

    /// **The invariant that keeps motion from lying.** A bar filling across a range
    /// asserts that the app knows when the range ends, and out here exactly one
    /// phase does: `speaking`, because `AssistantPlaybackClock` accumulates each
    /// queued audio frame's own duration. Every other phase would be drawing a
    /// duration nobody measured.
    ///
    /// Quantified over `allCases` so a sixth phase cannot quietly help itself to a
    /// progress bar.
    func testOnlySpeakingMayElapseBecauseOnlySpeakingHasAMeasuredEnd() {
        for phase in AmbientOrbPhase.allCases {
            let motion = phase.appearance(agentName: "Sam").motion
            if phase == .speaking {
                XCTAssertEqual(motion, .elapsing, "the reply's end is measured, so it may be drawn")
            } else {
                XCTAssertNotEqual(motion, .elapsing, "\(phase) has no duration the app knows")
            }
        }
    }

    /// `thinking` gets a different mechanism on purpose, not for want of one. There
    /// is deliberately no response deadline anywhere in this stack — a long tool
    /// call must not be cut off — so nothing knows when the answer arrives, and a
    /// treatment implying an end would be the orb asserting a duration it invented.
    func testThinkingIsIndeterminateBecauseNothingKnowsWhenAnAnswerArrives() {
        XCTAssertEqual(AmbientOrbPhase.thinking.appearance(agentName: "Sam").motion, .indeterminate)
    }

    /// `heard` is the CONNECT WAIT, and the connect wait was measured at 11–13 s.
    ///
    /// `AmbientState.orbPhase` reduces both `.heard(phrase:)` and `.connecting` onto
    /// this phase, and instrumentation put the first at 3–6 ms: what is on screen is
    /// always the second, for as long as the backend takes to create a provider
    /// session. A motionless glyph held for twelve seconds reads as a crash, which is
    /// why this is the second phase entitled to a pulse — and why it still may not
    /// have a bar, since that readiness time is unknown and moved 2.5 s across two
    /// consecutive runs.
    func testHeardIsIndeterminateBecauseItIsTheConnectWaitAndNothingTimesTheBackend() {
        XCTAssertEqual(AmbientOrbPhase.heard.appearance(agentName: "Sam").motion, .indeterminate)
    }

    /// **Every phase's motion is a decision, and this is where the type forces one.**
    ///
    /// The gap this closes is the one that cost twelve seconds of frozen orb: `.still`
    /// is the initialiser's DEFAULT, so a phase gets it by nobody choosing, and the
    /// invariants around it were a hand-written list of the phases somebody had
    /// already thought about. `heard` sat in that list for the whole time it was
    /// believed to be a 3 ms flash. An exhaustive `switch` over `allCases` means a
    /// sixth phase cannot inherit stillness — the compiler stops here until someone
    /// says what it depicts — and reverting one of these to the default is a failure
    /// rather than a diff nobody reads.
    ///
    /// `armed` and `listening` are still: an indicator that is always moving is one
    /// the user learns to ignore, and motion on a resting microphone would borrow the
    /// visual weight of an active conversation. The rationale for the other three is
    /// on the three tests either side of this one, which is where it belongs.
    func testNoPhaseIsStillMerelyBecauseNobodyChose() {
        for phase in AmbientOrbPhase.allCases {
            let motion = phase.appearance(agentName: "Sam").motion
            switch phase {
            case .armed, .listening:
                XCTAssertEqual(motion, .still, "\(phase) has nothing continuous to depict")
            case .heard, .thinking:
                XCTAssertEqual(motion, .indeterminate, "\(phase) is a wait whose end nothing can name")
            case .speaking:
                XCTAssertEqual(motion, .elapsing, "\(phase) is the one phase with a measured end")
            }
        }
    }

    /// **Motion must not outlive the thing it depicts.** An ended activity keeps
    /// rendering its final content for the whole dismissal window, so a bar still
    /// elapsing there — or a pulse still saying "working" — would be the same lie as
    /// an ended orb still saying "Listening", in a form that keeps moving. Asserted
    /// over both routes into that state, and over every phase the window could have
    /// ended from.
    func testAnEndedWindowHasNoMotionLeftWhicheverPhaseItEndedFrom() {
        XCTAssertEqual(AmbientOrbAppearance.ended(reason: AmbientEndedReason.capReached).motion, .still)
        for phase in AmbientOrbPhase.allCases {
            for (reason, stale) in [(AmbientEndedReason.capReached, false), (nil, true)] as [(String?, Bool)] {
                XCTAssertEqual(
                    AmbientOrbAppearance.forWindow(
                        phase: phase, endedReason: reason, isStale: stale, agentName: "Sam"
                    ).motion,
                    .still,
                    "\(phase) over(\(reason ?? "nil"), \(stale)) still has motion in it"
                )
            }
        }
    }

    // MARK: - Reduce Motion

    /// Scale is dropped entirely under Reduce Motion, so anything it is the only
    /// carrier of becomes invisible to that user. Every phase therefore has to
    /// differ from `armed` — the resting state — by colour as well.
    func testNoPhaseDiffersFromArmedByScaleAlone() {
        let armed = AmbientOrbPhase.armed.appearance(agentName: "Sam")
        for phase in AmbientOrbPhase.allCases where phase != .armed {
            XCTAssertNotEqual(
                phase.appearance(agentName: "Sam").fill,
                armed.fill,
                "\(phase) would vanish into armed under Reduce Motion"
            )
        }
    }

    // MARK: - the compact phase glyph

    /// The compact slot's emptiness rule, after the glyph's meaning narrowed:
    /// a resting or ended window shows NOTHING there, and a conversing one
    /// always shows SOMETHING — the word during the connect, a glyph once the
    /// session is live. `heard` carrying the word with NO glyph is the owner
    /// decision (2026-07-30): a mic in the one-glyph slot claims a session
    /// already listening, which the connect is not, so mic + ring first appear
    /// together at connect. An armed glyph would still put active-recording
    /// weight on a resting microphone, and an ended one would be the motion-
    /// outliving-its-subject lie in symbol form.
    func testTheCompactSlotShowsSomethingExactlyWhileConversing() {
        for phase in AmbientOrbPhase.allCases {
            let appearance = phase.appearance(agentName: "Sam")
            XCTAssertEqual(
                appearance.compactGlyph != nil || appearance.compactWord != nil,
                appearance.isConversing,
                "\(phase): the slot's contents and the conversation must arrive and leave together"
            )
        }
        let heard = AmbientOrbPhase.heard.appearance(agentName: "Sam")
        XCTAssertNil(heard.compactGlyph, "a mic during the connect would claim a session already listening")
        XCTAssertNotNil(heard.compactWord, "the connect wait's slot must not be empty either")
        XCTAssertNil(AmbientOrbAppearance.ended(reason: AmbientEndedReason.capReached).compactGlyph)
    }

    /// The symbol names are wire-adjacent UI vocabulary — a typo renders a
    /// blank image on a surface nobody snapshot-tests — so they are pinned as
    /// strings. `heard` pins nil rather than the mic it used to share with
    /// `listening` (owner decision, 2026-07-30 — the slot must not claim
    /// listening before the session is live; the word carries the connect);
    /// the exhaustive switch means a sixth phase must decide its glyph here
    /// rather than inherit one.
    func testTheGlyphVocabularyIsPinned() {
        for phase in AmbientOrbPhase.allCases {
            let glyph = phase.appearance(agentName: "Sam").compactGlyph
            switch phase {
            case .armed:
                XCTAssertNil(glyph)
            case .heard:
                XCTAssertNil(glyph, "the mic first appears at connect")
            case .listening:
                XCTAssertEqual(glyph, "mic.fill")
            case .thinking:
                XCTAssertEqual(glyph, "ellipsis")
            case .speaking:
                XCTAssertEqual(glyph, "speaker.wave.2.fill")
            }
        }
    }

    /// The compact word is the connect wait's text and nobody else's: it rides
    /// beside the leading orb, mic-free, and says the wait is work, and its
    /// removal — mic and ring arriving trailing in the same breath — is the
    /// "now actually listening" signal, which only signals if `heard` is the
    /// only phase that ever carries one. Quantified over `allCases` so a sixth
    /// phase cannot quietly borrow the word, and over `ended` because a word
    /// promising a wake on a closed window would be the
    /// motion-outliving-its-subject lie in text form.
    func testHeardIsTheOnlyPhaseWhoseCompactSlotCarriesAWord() {
        for phase in AmbientOrbPhase.allCases {
            let word = phase.appearance(agentName: "Sam").compactWord
            if phase == .heard {
                XCTAssertNotNil(word, "the connect wait must say the wait is work")
            } else {
                XCTAssertNil(word, "\(phase) has no connect wait to explain")
            }
        }
        XCTAssertNil(AmbientOrbAppearance.ended(reason: AmbientEndedReason.capReached).compactWord)
    }

    /// The two word regimes behind the one read the view takes. The connect
    /// word is CONTINUOUS — heard ignores the pulse flag, its own doctrine
    /// says why — while the conversing phases yield their word only while the
    /// flag says so, and the word IS the phase's `status` (the expanded
    /// line's exact vocabulary; owner ask, 2026-07-30). Armed and ended never
    /// yield one, whatever a stale flag claims: the flag is trusted only
    /// where a conversation is, so a leftover true renders nothing rather
    /// than a lie. Quantified over `allCases` so a sixth phase must pick its
    /// regime here.
    func testTheCompactWordReadFoldsBothRegimesHonestly() {
        for phase in AmbientOrbPhase.allCases {
            let appearance = phase.appearance(agentName: "Sam")
            let shown = appearance.compactWord(showingPhaseWord: true)
            let hidden = appearance.compactWord(showingPhaseWord: false)
            switch phase {
            case .heard:
                XCTAssertEqual(shown, appearance.compactWord, "the connect word is continuous, not pulsed")
                XCTAssertEqual(hidden, appearance.compactWord, "a hide-flip must not blank the connect word")
            case .listening, .thinking, .speaking:
                XCTAssertEqual(shown, appearance.status, "the pulse speaks the expanded line's own word")
                XCTAssertNil(hidden, "between pulses the slot is orb-only")
            case .armed:
                XCTAssertNil(shown, "a resting window shows no word, whatever the flag claims")
                XCTAssertNil(hidden)
            }
        }
        let ended = AmbientOrbAppearance.ended(reason: AmbientEndedReason.capReached)
        XCTAssertNil(ended.compactWord(showingPhaseWord: true), "an ended orb wears no word — a stale flag renders nothing")
    }

    // MARK: - Aurora palette

    /// Colour stays the primary carrier of state (it is the only channel that
    /// survives the minimal presentation), so the aurora body gradients must be
    /// pairwise distinct — and deliberately STRICTER than the flat fills, which
    /// sanction one shared colour (`heard` settling into `listening`). The
    /// palette is what the large presentations read, so no exception is
    /// sanctioned here.
    func testNoTwoPhasesShareACoreGradient() {
        let stops = AmbientOrbPhase.allCases.map { $0.appearance(agentName: "Sam").palette.coreStops }
        XCTAssertEqual(Set(stops).count, stops.count, "two phases render the same aurora body")
    }

    /// At orb sizes the body's SHAPE carries the phase alongside its colour:
    /// each live phase owns a distinct blob silhouette, and the identity-swap
    /// cross-dissolve between them is the only morph an out-of-process surface
    /// can render. Two phases sharing a seed would erase that morph for
    /// exactly one transition — the kind of gap nobody notices until device.
    func testEveryLivePhaseOwnsItsOwnBlobSilhouette() {
        let seeds = AmbientOrbPhase.allCases.map { $0.appearance(agentName: "Sam").palette.blobSeed }
        XCTAssertEqual(Set(seeds).count, seeds.count, "two phases render the same blob silhouette")
    }

    /// Ended means nothing here is live: no glow, and a body no live phase uses.
    func testEndedIsHaloFreeAndDistinctFromEveryLivePhase() {
        let ended = AmbientOrbAppearance.ended(reason: "x").palette
        XCTAssertEqual(ended.haloStrength, 0, "an ended orb must not glow")
        for phase in AmbientOrbPhase.allCases {
            XCTAssertNotEqual(ended.coreStops, phase.appearance(agentName: "Sam").palette.coreStops)
        }
    }

    /// The other half of "an ended orb must not glow": every live phase must,
    /// because `haloStrength == 0` is more than a dimmer setting — it is how the
    /// orb view recognises graphite. Its amplitude switch renders a zero-halo
    /// palette as a perfect circle, stillness and geometry both saying "over",
    /// so a live phase retuned to no glow would not merely dim: it would freeze
    /// into ended's silhouette while its status line still claims a live
    /// microphone.
    func testEveryLivePhaseGlowsBecauseNoGlowIsEndedsSignature() {
        for phase in AmbientOrbPhase.allCases {
            XCTAssertGreaterThan(phase.appearance(agentName: "Sam").palette.haloStrength, 0,
                                 "\(phase) would render as ended's still circle")
        }
    }

    /// Armed is ambient presence, not active recording — quantified as numbers so
    /// a retune cannot silently invert it.
    func testArmedGlowsDimmerThanEveryConversingPhase() {
        let armed = AmbientOrbPhase.armed.appearance(agentName: "Sam").palette
        for phase in AmbientOrbPhase.allCases {
            let appearance = phase.appearance(agentName: "Sam")
            guard appearance.isConversing else { continue }
            XCTAssertLessThan(armed.haloStrength, appearance.palette.haloStrength,
                              "\(phase) should out-glow the resting orb")
        }
    }

    /// The orb view draws an `AngularGradient` from these stops and indexes the
    /// first one directly, so a palette that runs out of stops must fail here
    /// rather than crash the widget process.
    func testEveryPaletteCarriesAtLeastTwoCoreStops() {
        var palettes = AmbientOrbPhase.allCases.map { $0.appearance(agentName: "Sam").palette }
        palettes.append(AmbientOrbAppearance.ended(reason: "x").palette)
        for palette in palettes {
            XCTAssertGreaterThanOrEqual(palette.coreStops.count, 2, "a gradient needs at least two stops")
        }
    }

    /// `haloStrength` documents itself as a 0…1 opacity; assert the claim so a
    /// retune cannot quietly step outside the range the views hand to `opacity`.
    func testEveryHaloStrengthIsAnOpacity() {
        var palettes = AmbientOrbPhase.allCases.map { $0.appearance(agentName: "Sam").palette }
        palettes.append(AmbientOrbAppearance.ended(reason: "x").palette)
        for palette in palettes {
            XCTAssertTrue((0...1).contains(palette.haloStrength),
                          "haloStrength \(palette.haloStrength) is not an opacity")
        }
    }

    /// The in-app voice beacon draws its engine glyph in plain white directly
    /// over these gradients — "the aurora body is never light" is a claim in
    /// that view's code, and this is where the claim becomes a number: every
    /// preset body stop stays below a relative-luminance cap.
    ///
    /// The cap is deliberately loose (the glyph also carries a black shadow);
    /// what it must catch is a retuned preset drifting toward white, which
    /// would wash the glyph out on every surface at once. Quantified over a
    /// hand-kept list because the presets have no `allCases` — a new preset
    /// must be added here, or it ships unpinned under a white glyph.
    func testEveryPresetBodyStopStaysDarkEnoughForAWhiteGlyph() {
        let presets: [(String, AuroraPalette)] = [
            ("armedEmber", .armedEmber),
            ("violetSurge", .violetSurge),
            ("calmAurora", .calmAurora),
            ("amberThinking", .amberThinking),
            ("tealSpeaking", .tealSpeaking), // tealDone IS this value, so it is covered
            ("graphite", .graphite)
        ]
        for (name, palette) in presets {
            for (index, stop) in palette.coreStops.enumerated() {
                var red: CGFloat = 0, green: CGFloat = 0, blue: CGFloat = 0, alpha: CGFloat = 0
                XCTAssertTrue(UIColor(stop).getRed(&red, green: &green, blue: &blue, alpha: &alpha),
                              "\(name) stop \(index) yields no RGB reading to assert over")
                XCTAssertLessThan(relativeLuminance(red: red, green: green, blue: blue), 0.75,
                                  "\(name) stop \(index) is light enough to wash out the white engine glyph")
            }
        }
    }

    /// WCAG relative luminance over sRGB components — the standard measure of
    /// how light a colour reads, so the cap above means one thing.
    private func relativeLuminance(red: CGFloat, green: CGFloat, blue: CGFloat) -> Double {
        func linear(_ component: CGFloat) -> Double {
            let c = Double(component)
            return c <= 0.03928 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
    }

    // MARK: - Receipt

    func testReceiptSaysDurationAndExchanges() {
        let armed = Date(timeIntervalSince1970: 0)
        let ended = armed.addingTimeInterval(42 * 60)
        XCTAssertEqual(AmbientReceipt.line(armedAt: armed, endedAt: ended, exchangeCount: 6),
                       "Listened 42m · 6 exchanges")
    }

    func testReceiptSingularExchangeAndSubMinuteDuration() {
        let armed = Date(timeIntervalSince1970: 0)
        XCTAssertEqual(AmbientReceipt.line(armedAt: armed, endedAt: armed.addingTimeInterval(20), exchangeCount: 1),
                       "Listened under a minute · 1 exchange")
    }

    func testReceiptOmitsExchangesWhenNoneHappened() {
        let armed = Date(timeIntervalSince1970: 0)
        XCTAssertEqual(AmbientReceipt.line(armedAt: armed, endedAt: armed.addingTimeInterval(300), exchangeCount: 0),
                       "Listened 5m")
    }

    /// The ambient leash legally runs for hours, so a minutes-only rendering
    /// would report a full window as a three-digit minute count.
    func testReceiptSpeaksHoursForLongWindows() {
        let armed = Date(timeIntervalSince1970: 0)
        XCTAssertEqual(AmbientReceipt.line(armedAt: armed, endedAt: armed.addingTimeInterval(7500), exchangeCount: 2),
                       "Listened 2h 5m · 2 exchanges")
        XCTAssertEqual(AmbientReceipt.line(armedAt: armed, endedAt: armed.addingTimeInterval(7200), exchangeCount: 0),
                       "Listened 2h")
    }

    /// No endedAt means an old-build payload or a window that never published a
    /// final update — there is no honest duration, so there is no receipt.
    func testReceiptRefusesWithoutAnHonestEnd() {
        let armed = Date(timeIntervalSince1970: 0)
        XCTAssertNil(AmbientReceipt.line(armedAt: armed, endedAt: nil, exchangeCount: 3))
        XCTAssertNil(AmbientReceipt.line(armedAt: armed, endedAt: armed, exchangeCount: 3))
        XCTAssertNil(AmbientReceipt.line(armedAt: armed, endedAt: armed.addingTimeInterval(-5), exchangeCount: 3))
    }
}
