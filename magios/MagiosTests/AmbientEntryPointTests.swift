import XCTest
@testable import Magician

/// The arm-time phrase source and the leash.
///
/// Both are pure over value types, which is the whole reason they were factored
/// out of the entry point: the wiring around them needs a scene, an App Group
/// and a microphone, and the two decisions that actually matter — *what will the
/// microphone wake on* and *when does it turn itself off* — need none of those.
final class AmbientActivationPhrasesTests: XCTestCase {

    /// The construction the backend performs in
    /// `voice_addressing.rs` (`VoiceAddressing::activation_phrases`), reproduced
    /// locally from the cached identity. Agreement is the point: the local gate
    /// and the server's gate must listen for the same words, or a locally
    /// matched wake is discarded server-side as unaddressed.
    func testEveryAdvertisedNameBecomesAPrefixedPhrase() {
        let identity = PrimaryAgentSiriIdentity(agentID: "a", name: "Atlas", aliases: ["Nova"])

        XCTAssertEqual(
            AmbientActivationPhrases.forArming(identity: identity),
            ["Hey Atlas", "Hey Nova"]
        )
    }

    /// The prefix is the server's contract, not a decoration, so an alias that
    /// already carries it must not end up doubled — a phrase nobody will say.
    /// The server strips it for the same reason.
    func testAnAliasThatAlreadyCarriesThePrefixIsNotDoubled() {
        let identity = PrimaryAgentSiriIdentity(agentID: "a", name: "Sam", aliases: ["hey sam", "HEY Nova"])

        XCTAssertEqual(
            AmbientActivationPhrases.forArming(identity: identity),
            ["Hey Sam", "Hey Nova"],
            "stripping must be case-insensitive, and must collapse the alias onto the canonical name"
        )
    }

    /// Punctuation is not a word boundary the wake model knows about, and the
    /// backend tokenises on non-alphanumerics. Diverging here would arm a phrase
    /// the server would never gate on.
    func testNamesAreTokenisedOnNonAlphanumericsLikeTheBackend() {
        let identity = PrimaryAgentSiriIdentity(agentID: "a", name: "  Sam-the-Magician!  ", aliases: [])

        XCTAssertEqual(AmbientActivationPhrases.forArming(identity: identity), ["Hey Sam the Magician"])
    }

    /// **The degradation, and it is deliberate.** No identity means no phrase,
    /// which `AmbientController.arm` refuses. There is no hardcoded fallback
    /// name: arming a live microphone on a word the user never chose is worse
    /// than not arming, because the phrase IS the gate.
    func testAnEmptyIdentityYieldsNoPhraseRatherThanAnInventedOne() {
        XCTAssertEqual(AmbientActivationPhrases.forArming(identity: nil), [])
        XCTAssertEqual(
            AmbientActivationPhrases.forArming(
                identity: PrimaryAgentSiriIdentity(agentID: "a", name: "  ", aliases: ["", "!!"])
            ),
            []
        )
    }

    /// The measured numbers have to survive the construction. This is the set the
    /// picker's readout and `AmbientPhraseSet.notes` both report on, so a
    /// construction that produced phrases the table has no entry for would turn a
    /// measured row into an unmeasured one and lose the number.
    func testThePrefixedFormIsTheOneTheMatrixMeasured() {
        let phrases = AmbientActivationPhrases.forArming(
            identity: PrimaryAgentSiriIdentity(agentID: "a", name: "Sam", aliases: ["Magician"])
        )

        XCTAssertEqual(
            VoskWakeSpotter.assessment(of: phrases[0]),
            .measured(nearMissFalseAcceptPercent: 53, syntheticTrueAcceptPercent: 96, note: VoskWakeSpotter.assessment(of: phrases[0]).note!),
            "“Hey Sam” is the worst prefixed row measured"
        )
        XCTAssertEqual(
            VoskWakeSpotter.assessment(of: phrases[1]),
            .measured(nearMissFalseAcceptPercent: 20, syntheticTrueAcceptPercent: 92, note: VoskWakeSpotter.assessment(of: phrases[1]).note!),
            "“Hey Magician” is the best `hey` row measured"
        )
        // Both carry a number, and neither is refused: 53% and 20% are both
        // reported, because there is no defensible line between them to refuse on.
        XCTAssertNotNil(VoskWakeSpotter.assessment(of: phrases[0]).note)
        XCTAssertNotNil(VoskWakeSpotter.assessment(of: phrases[1]).note)
    }
}

final class AmbientLeashTests: XCTestCase {

    /// Every option is finite, including the one whose label is not. An
    /// unbounded window would have no end anything could enforce: the cap is
    /// what `AmbientArm` stores and what the orb's `staleDate` is set to, and
    /// that stale date is the only backstop that survives the app being killed
    /// past its cap.
    func testEveryLeashHasAFiniteCap() {
        for leash in AmbientLeash.allCases {
            XCTAssertGreaterThan(leash.capSeconds, 0, "\(leash.rawValue) has no enforceable end")
        }
    }

    /// The longest leash is bounded by ActivityKit rather than by taste: the
    /// system ends a Live Activity after eight hours, and past that the orb —
    /// the only disarm control reachable from outside the app — is gone.
    func testTheLongestLeashDoesNotOutliveTheOrb() {
        XCTAssertLessThanOrEqual(
            AmbientLeash.untilStopped.capSeconds,
            8 * 60 * 60,
            "a cap past the Live Activity limit guarantees armed minutes with no way to stop them from outside"
        )
        XCTAssertEqual(
            AmbientLeash.allCases.map(\.capSeconds).max(),
            AmbientLeash.untilStopped.capSeconds
        )
    }

    /// The raw values are persisted in the App Group, so a rename is a silent
    /// reset of the user's choice rather than a compile error.
    func testRawValuesAreTheStableStoredContract() {
        XCTAssertEqual(AmbientLeash.thirtyMinutes.rawValue, "30m")
        XCTAssertEqual(AmbientLeash.twoHours.rawValue, "2h")
        XCTAssertEqual(AmbientLeash.untilStopped.rawValue, "until_stopped")
    }

    func testLongestLeashUsesThePublicStopListeningLabel() {
        XCTAssertTrue(AmbientLeash.untilStopped.detail.contains("Stop listening"))
        XCTAssertFalse(AmbientLeash.untilStopped.detail.contains("Disarm"))
    }
}

final class AmbientPublicCopyTests: XCTestCase {
    private var magiosRoot: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
    }

    private func source(_ relativePath: String) throws -> String {
        try String(
            contentsOf: magiosRoot.appendingPathComponent(relativePath),
            encoding: .utf8
        )
    }

    func testVisibleAmbientActionsUseTalkAndStopListening() throws {
        let activity = try source("MagiosWidgets/AmbientLiveActivity.swift")
        let startIntent = try source("MagiosIntents/ArmAmbientIntent.swift")
        let stopIntent = try source("Shared/DisarmAmbientIntent.swift")
        let extendIntent = try source("Shared/ExtendAmbientIntent.swift")

        XCTAssertTrue(activity.contains(#"Label("Stop", systemImage: "stop.fill")"#))
        XCTAssertTrue(activity.contains(#".accessibilityLabel("Stop listening")"#))
        XCTAssertFalse(activity.contains(#"Label("Disarm", systemImage: "stop.fill")"#))
        XCTAssertTrue(startIntent.contains(#"LocalizedStringResource = "Talk to Magican""#))
        XCTAssertFalse(startIntent.contains(#"LocalizedStringResource = "Arm ambient listening""#))
        XCTAssertTrue(stopIntent.contains(#"LocalizedStringResource = "Stop ambient listening""#))
        XCTAssertFalse(stopIntent.contains(#"LocalizedStringResource = "Disarm ambient listening""#))
        XCTAssertTrue(extendIntent.contains(#"LocalizedStringResource = "Extend ambient listening""#))
        XCTAssertTrue(extendIntent.contains("openAppWhenRun = false"))
    }

    func testLiveActivityLeashUsesOneLineDigitalCountdownOnEveryTextSurface() throws {
        let activity = try source("MagiosWidgets/AmbientLiveActivity.swift")

        XCTAssertTrue(activity.contains("timerInterval: now ... max(now, expiresAt)"))
        XCTAssertTrue(activity.contains("countsDown: true"))
        XCTAssertTrue(activity.contains("showsHours: false"))
        XCTAssertTrue(activity.contains(".lineLimit(1)"))
        XCTAssertEqual(
            activity.components(separatedBy: "AmbientRemainingLeashText(expiresAt:").count - 1,
            2,
            "the lock screen and expanded island must share the non-localizing countdown"
        )
        XCTAssertFalse(
            activity.contains("Text(context.attributes.expiresAt, style: .timer)"),
            "the date-style timer may localize to prose such as ‘28 minutes’ and wrap"
        )
        XCTAssertEqual(
            activity.components(separatedBy: "AmbientExtendControl(").count - 1,
            2,
            "the Lock Screen and expanded Dynamic Island both expose the bounded extension"
        )
        XCTAssertTrue(activity.contains("Button(intent: ExtendAmbientIntent())"))
        XCTAssertTrue(activity.contains(#"Label("+30 min", systemImage: "clock.badge.plus")"#))
        XCTAssertEqual(
            activity.components(separatedBy: "context.state.effectiveExpiresAt(").count - 1,
            2,
            "both presentations must prefer the mutable expiry over the immutable launch attribute"
        )
    }

    func testEverySystemSurfaceUsesOneTalkActionAndHidesTheLegacyLauncher() throws {
        let widget = try source("MagiosWidgets/MagiosWidgets.swift")
        let legacyLauncher = try source("MagiosIntents/StartVoiceIntent.swift")
        let settings = try source("Magios/SettingsView.swift")
        let entryPoint = try source("Magios/AmbientEntryPoint.swift")
        let activity = try source("Magios/AmbientActivity.swift")
        let liveActivity = try source("MagiosWidgets/AmbientLiveActivity.swift")
        let app = try source("Magios/App.swift")
        let sharedActions = try source("Shared/SharedActions.swift")
        let controlSymbol = try source(
            "MagiosWidgets/Assets.xcassets/MagicanTalkControl.symbolset/magican-talk-control.svg"
        )
        let controlSymbolManifest = try source(
            "MagiosWidgets/Assets.xcassets/MagicanTalkControl.symbolset/Contents.json"
        )

        XCTAssertTrue(widget.contains(#"Button(intent: ArmAmbientIntent())"#))
        XCTAssertTrue(widget.contains(#"Text("MAGICAN")"#))
        XCTAssertFalse(widget.contains(#"Text("U U A A")"#))
        XCTAssertTrue(
            widget.contains(#".configurationDisplayName("Magican at a glance")"#),
            "the consolidated widget is not a second Talk launcher even though Talk remains one of its controls"
        )
        let shortcuts = try source("MagiosIntents/MagiosIntents.swift")
        XCTAssertTrue(shortcuts.contains("struct TalkToMagicanIntent"))
        XCTAssertTrue(shortcuts.contains(#"shortTitle: "Talk to Magican""#))
        XCTAssertFalse(widget.contains(#".widgetURL(SharedActions.configuredVoiceURL)"#))

        let control = try XCTUnwrap(
            widget.split(separator: "struct MagiosAmbientControl", maxSplits: 1).last?
                .split(separator: "@main", maxSplits: 1).first
        )
        XCTAssertTrue(
            control.contains(#"Label("Talk to Magican", image: "MagicanTalkControl")"#)
        )
        XCTAssertFalse(widget.contains("MagicanTalkControlGlyph"))
        XCTAssertFalse(widget.contains("ZStack(alignment: .bottomTrailing)"))
        XCTAssertTrue(controlSymbol.contains(#"id="Regular-S""#))
        XCTAssertTrue(controlSymbol.contains(#"id="Ultralight-S""#))
        XCTAssertTrue(controlSymbol.contains(#"id="Black-S""#))
        XCTAssertTrue(controlSymbol.contains("Template v.3.0"))
        XCTAssertTrue(controlSymbolManifest.contains(#""filename" : "magican-talk-control.svg""#))
        XCTAssertTrue(controlSymbolManifest.contains(#""idiom" : "universal""#))
        XCTAssertFalse(control.contains(#"Image("MagicanControlGlyph")"#))
        XCTAssertFalse(control.contains(".renderingMode(.template)"))
        XCTAssertFalse(
            control.contains(#"systemImage: "waveform""#),
            "Control Center must use Magican's lowercase-m identity, not the generic voice icon shared by other apps"
        )
        XCTAssertTrue(widget.contains("SharedActions.ambientControlKind"))
        XCTAssertTrue(
            sharedActions.contains(
                #"ambientControlKind = "ai.magicbeans.Magican.ambient-control""#
            )
        )
        XCTAssertTrue(app.contains("ControlCenter.shared.reloadControls"))
        XCTAssertTrue(app.contains("SharedActions.ambientControlKind"))
        XCTAssertFalse(widget.contains("MagiosVoiceControl"))
        XCTAssertTrue(widget.contains("MagiosAmbientControl()"))
        XCTAssertTrue(legacyLauncher.contains("static var isDiscoverable: Bool = false"))
        XCTAssertTrue(legacyLauncher.contains("SharedActions.PendingAction.ambientArm"))
        XCTAssertFalse(legacyLauncher.contains("SharedActions.PendingAction.voiceConfigured"))
        XCTAssertTrue(liveActivity.contains("Link(destination: SharedActions.ambientURL)"))
        XCTAssertFalse(liveActivity.contains("Link(destination: SharedActions.configuredVoiceURL)"))
        XCTAssertTrue(app.contains("default:\n                // Mode-neutral legacy links"))
        XCTAssertTrue(app.contains("AppActions.shared.requestAmbientArm()"))
        XCTAssertFalse(settings.contains("Quick Talk"))
        XCTAssertTrue(settings.contains("await AmbientEntryPoint.talk()"))
        XCTAssertTrue(entryPoint.contains("_ = await controller.talkNow()"))
        XCTAssertTrue(activity.contains(#"let title = "Starting conversation""#))
        XCTAssertFalse(activity.localizedCaseInsensitiveContains("heard you"))
    }

    // MARK: - Wake spellings for a name the recogniser cannot arm

    /// The spotter is grammar-constrained: a word missing from the model's
    /// vocabulary is dropped with only a log, so the phrase silently never
    /// fires. Spellings therefore replace the advertised names.
    func testWakeSpellingsReplaceANameTheRecogniserCannotArm() {
        let identity = PrimaryAgentSiriIdentity(
            agentID: "a",
            name: "Magican",
            aliases: [],
            wakeSpellings: ["magical", "magician"]
        )

        XCTAssertEqual(
            AmbientActivationPhrases.forArming(identity: identity),
            ["Hey magical", "Hey magician"]
        )
        // Display and Siri stay open-vocabulary and keep the real name.
        XCTAssertEqual(identity.advertisedNames, ["Magican"])
    }

    /// The override is opt-in: an in-lexicon name still arms as spelled.
    func testAbsentWakeSpellingsLeaveArmingOnTheNameAndAliases() {
        let identity = PrimaryAgentSiriIdentity(
            agentID: "a", name: "Magician", aliases: [], wakeSpellings: ["  ", ""]
        )

        XCTAssertEqual(
            AmbientActivationPhrases.forArming(identity: identity),
            ["Hey Magician"]
        )
    }

}
