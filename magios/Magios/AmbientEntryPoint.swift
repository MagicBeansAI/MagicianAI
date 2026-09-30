import Foundation

/// Where the wake phrases come from at ARM time.
///
/// ## The obvious source is not available, and the reason matters
///
/// `RealtimeVoiceProtocol.Addressing.activationPhrases` is negotiated in
/// `session.ready` — that is, *during a call*. The spotter needs phrases
/// **before** any call exists, so that field cannot be the arm-time source. It
/// is still the authority once a conversation is running; this is what stands in
/// for it while nothing is.
///
/// ## What it is instead, and why it is not an invention
///
/// The backend builds `activation_phrases` from the primary agent's aliases plus
/// its canonical name, strips a leading "hey" from each, and returns
/// `"Hey <name>"` — see `magician-media/src/media_rails/voice_addressing.rs`
/// (`VoiceAddressing::new` / `activation_phrases`) and its caller in
/// `voice_control_handler.rs`. `PrimaryAgentSiriIdentity` holds `name` and
/// `aliases` from the *same* `/v2/agents` primary record, cached in the App
/// Group so intents can read it out of process.
///
/// So this reproduces the server's own construction from the cached identity.
/// The result is the same phrase set `session.ready` will later negotiate for
/// the same names, which is the property that matters: the local gate and the
/// server's gate listen for the same words, so a wake that fires locally is not
/// then discarded server-side as unaddressed.
///
/// **The "Hey " prefix is the server's contract, not a decoration added here** —
/// and the measurements happen to support it independently. All six bare names
/// in the matrix false-accept at 33-80% on finalised results, and every one of
/// them was measured against its own prefixed form without a single reversal:
/// five improved and one tied. (The stronger claim this comment used to make —
/// that a bare word was the only thing in the matrix to fire on ordinary
/// unrelated speech — was a partials artifact and is retired; on finals every
/// row is 0/8 there.) See `VoskWakeSpotter.assessment(of:)`.
///
/// ## How it degrades
///
/// An empty cache yields an **empty phrase set**, and `AmbientController.arm`
/// refuses it with a reason that says so. There is deliberately no fallback
/// name: a hardcoded one would arm a microphone on a word the user never chose,
/// which is worse than not arming at all — the wake phrase is the entire gate
/// between a live microphone and the network. The cache is refreshed on every
/// foreground by `PrimaryAgentSiriAdvertiser`, and arming opens the app, so the
/// refusal self-heals on the next attempt.
///
/// Nothing is filtered on measured accuracy here. A phrase built from any name
/// still ends up in this set, is still armed, and what is known about it — a
/// measured rate, or plainly that nobody has measured it — is still carried on
/// `AmbientPhraseSet.notes`, though rendered nowhere post-arm; the user reads it
/// pre-arm in Settings. Surfacing is this feature's answer; refusing on a
/// number is a product decision nobody has taken.
enum AmbientActivationPhrases {

    /// The address prefix the backend puts in front of every name. Spelled once,
    /// here, because the whole value of this type is agreeing with the server.
    static let prefix = "Hey"

    /// The phrase set to arm the spotter with, or empty when the identity cache
    /// has nothing usable in it.
    ///
    /// Pure, and taking the identity rather than reading the store, so the
    /// derivation is assertable without an App Group.
    static func forArming(identity: PrimaryAgentSiriIdentity?) -> [String] {
        var seen = Set<String>()
        var phrases: [String] = []
        // `armingNames`, not `advertisedNames`: a name the wake model has no
        // vocabulary entry for is dropped from the grammar and can never fire,
        // so the definition may name in-lexicon spellings to arm in its place.
        for advertised in identity?.armingNames ?? [] {
            // Tokenised on non-alphanumerics, matching `voice_addressing.rs`'s
            // `tokens()` exactly — including that an apostrophe splits — so the
            // two constructions cannot disagree about where a name begins.
            var words = advertised
                .split(whereSeparator: { !$0.isLetter && !$0.isNumber })
                .map(String.init)
            // An alias may already carry the prefix. Left alone it would become
            // a doubled one, which is not a phrase anybody will ever say; the
            // server strips it for the same reason.
            if words.first?.lowercased() == prefix.lowercased() { words.removeFirst() }
            guard !words.isEmpty else { continue }
            let name = words.joined(separator: " ")
            guard seen.insert(name.lowercased()).inserted else { continue }
            phrases.append("\(prefix) \(name)")
        }
        return phrases
    }
}

/// The one-time nudge from the in-app control towards the invisible one.
///
/// ## Why it exists, and why it is shown when it is
///
/// The two entry points are not equals. Arming from Settings is the *discoverable*
/// one and the Control Center control is the *good* one — after it, every later
/// wake word and every later disarm runs with no visible transition for the life of
/// the window. A user who only ever finds the Settings button pays a trip through
/// the app for something that was designed to cost nothing.
///
/// It fires the first time a window is armed **from inside the app**, rather than
/// on first launch. A launch interstitial about Control Center reaches someone who
/// does not yet know what ambient mode is, which is the definition of a prompt
/// people dismiss without reading; arming is the moment the user has just
/// demonstrated they want the feature, and "next time, without opening Magican" is
/// then an answer to a question they have actually asked. It also means the person
/// who found the control already never sees it.
///
/// Shown once, ever. The flag is persisted in the App Group store, alongside
/// `SiriPhrasePresentation.customPhraseKey`, and it is set the moment the prompt is
/// raised rather than when it is dismissed — a prompt the user swipes away has been
/// shown, and re-raising it is the nagging this is written to avoid.
enum AmbientControlCenterHint {

    /// Versioned, so a future revision of the copy can decide for itself whether it
    /// is worth showing again rather than inheriting a `true` from this one.
    static let shownKey = "ambient.controlCenterHintShown.v1"

    static let title = "Talk without opening Magican again"

    /// Names the control by its label so the user can search for it, and says
    /// plainly what it buys, because "add a control" on its own is a chore with no
    /// stated reward.
    static let message = """
        Add “Talk to Magican” to Control Center, the Lock Screen, or the Action button. \
        The first tap starts talking right away and opens Magican for a moment — iOS \
        requires it — then wake-word follow-ups work without leaving what you're doing.
        """

    /// Pure, so "once, and only after a window actually opened" is assertable
    /// without a view.
    ///
    /// `armSucceeded` matters: a refused arm — no activation phrase, a battery below
    /// the floor, an observation already holding the microphone — must not spend the
    /// one showing. The user has not seen the feature work yet, so a nudge towards a
    /// faster way to do the thing that just failed is noise.
    static func shouldShow(alreadyShown: Bool, armSucceeded: Bool) -> Bool {
        armSucceeded && !alreadyShown
    }
}

/// The app-side lifecycle of ambient mode: the three things that have to happen
/// when Magican comes up, in the one order that is safe.
///
/// It is a type rather than three calls in `App.handleActivation` because the
/// ordering below is load-bearing and has to be stated somewhere a reader will
/// find it, and because the phrase and leash resolution is worth asserting
/// without a scene.
@MainActor
enum AmbientEntryPoint {

    /// `reconcileOnLaunch` is named for exactly when it may run, and running it
    /// on a later foreground would be actively harmful: it CLEARS the pending
    /// disarm record, which on any activation after the first is the resume
    /// path's only evidence that the user tapped Disarm on the orb.
    private static var didReconcile = false

    /// Called on launch, on every foreground, and on every deep link.
    ///
    /// The three steps are ordered, and two of the orderings are the point:
    ///
    /// 1. **Reconcile, once.** Collects an orb that outlived its process and
    ///    clears an arm record and a disarm request left by a dead one.
    /// 2. **Consume a pending disarm.** Task 7's second self-heal leg: the app
    ///    may have been suspended when the orb's Disarm button was tapped, in
    ///    which case the App Group record is the only evidence it happened. An
    ///    uncalled backstop is not a backstop.
    /// 3. **Consume a pending extension.** Stop goes first, so simultaneous
    ///    controls can never keep alive a window the user also asked to stop.
    /// 4. **Talk, if an intent asked for it.** Strictly *after* step 2, and that
    ///    is not cosmetic. A disarm request can be outstanding when the user taps
    ///    Arm — the intent leaves one standing whenever it could not get an
    ///    acknowledgement — and consuming it after the arm would tear down the
    ///    window the user just opened, using an answer to a question about a
    ///    previous one.
    static func handleActivation() async {
        reconcileOnceAtLaunch()
        await AmbientController.shared.consumePendingDisarmRequest()
        AmbientController.shared.consumePendingExtensionRequest()
        await talkIfRequested()
    }

    private static func reconcileOnceAtLaunch() {
        guard !didReconcile else { return }
        didReconcile = true
        AmbientController.shared.reconcileOnLaunch()
    }

    /// Start the conversation `ArmAmbientIntent` asked for, if it did.
    ///
    /// The request is a latch on `AppActions` rather than a bumped request id
    /// with a view observing it, because arming needs no view: it is a controller
    /// action, and on a cold launch the pending App Group action is consumed
    /// before SwiftUI has installed any `.onReceive` subscriber. A latch read in
    /// the same activation cannot lose that race.
    ///
    /// **This runs in the foreground, and must.** iOS refuses to activate a
    /// recording audio session from the background, so the session is opened
    /// here, once, while Magican is visible — and never deactivated. That is the
    /// whole reason `ArmAmbientIntent` is an `OpenIntent`: the one visible launch
    /// buys every later wake and every later disarm an invisible one.
    static func talkIfRequested() async {
        guard AppActions.shared.consumeAmbientArmRequest() else { return }
        await talk()
    }

    /// The unified system action: open an ambient window if needed, then begin
    /// its first conversation immediately. A later conversation still starts
    /// with the wake phrase because `AmbientController` returns the same window
    /// to spotting when this call ends.
    ///
    /// Reusing a live window is important for Action Button and Lock Screen
    /// taps: arming twice is deliberately refused by the controller, but a tap
    /// while the window is quietly available should still start a turn. If the
    /// window is in its bounded post-call resume cooldown, `talkNow()` waits for
    /// that transition rather than dropping the explicit user action.
    static func talk() async {
        let controller = AmbientController.shared
        if !controller.windowIsLive {
            await arm()
        }
        _ = await controller.talkNow()
    }

    /// Open a window, from wherever asked.
    ///
    /// **One construction of the phrase set and the leash, for both entry points.**
    /// The intent path and the Settings control resolve exactly the same two inputs,
    /// and a second copy is how they come to disagree — a leash read from a stale
    /// snapshot, or a phrase set built without the "Hey " prefix the backend
    /// contract requires (`AmbientActivationPhrases`). Every refusal is therefore
    /// identical from either door: an empty identity cache, a phrase the on-device
    /// model has never heard, a battery below the floor, an observation already
    /// holding the microphone.
    ///
    /// **Both callers are in the foreground, and both must be.** iOS refuses to
    /// activate a recording audio session from the background, which is why
    /// `ArmAmbientIntent` is an `OpenIntent` — and it is also why an in-app control
    /// needs nothing special to be safe here. It is the *easy* case; the intent is
    /// the one that had to buy its foreground.
    static func arm() async {
        await AmbientController.shared.arm(
            phrases: AmbientActivationPhrases.forArming(identity: PrimaryAgentSiriIdentityStore.load()),
            capSeconds: AudioSettings.shared.ambientLeash.capSeconds
        )
    }
}
