import AppIntents

/// The single destination exposed by the Talk control. An AppEnum rather than
/// a magic string for the same reason `VoiceControlDestination` is one:
/// `OpenIntent` requires an app value target, and Control Center persists that
/// value with the configured control.
enum AmbientControlDestination: String, AppEnum {
    case listeningWindow

    static var typeDisplayRepresentation = TypeDisplayRepresentation("Magican talk destination")
    static var caseDisplayRepresentations: [AmbientControlDestination: DisplayRepresentation] = [
        .listeningWindow: "Talk to Magican"
    ]
}

/// Starts "Talk to Magican" from a widget, Control Center, the Lock Screen, the
/// Action Button, or Shortcuts.
///
/// **It is an `OpenIntent`, and the visible launch is the design rather than a
/// compromise.** iOS refuses to activate a recording audio session from the
/// background — Apple's engineers have said so four times across a decade, most
/// recently after `AudioRecordingIntent` had already shipped — so capture cannot
/// be *started* by a background-launched intent. Apple's own prescription (DTS
/// thread 826462) is to have the `audio` background mode and only ever activate
/// the session in the foreground; activate once while visible, never deactivate,
/// and the engine can then be started and stopped from the background
/// indefinitely.
///
/// So the first explicit tap starts a conversation immediately while paying one
/// visible launch per ambient window. When that conversation ends, the same
/// window remains available: every wake hit and the orb's Stop button run with
/// no visible transition for the life of the window. Do not try to make the
/// first launch invisible: a background `AppIntent` here would arm a window
/// whose microphone could never open, which reads as an orb over nothing.
///
/// Shaped exactly like `StartVoiceIntent`: `perform()` persists the pending
/// action *before* iOS activates Magican, so a cold launch cannot outrun SwiftUI
/// setup. The app consumes it in `AmbientEntryPoint.handleActivation()`, which
/// deliberately drains any outstanding disarm request first.
///
/// This source is compiled into both Magios and MagiosWidgets — `OpenIntent`
/// launch controls need membership in the containing app and the widget
/// extension alike.
struct ArmAmbientIntent: OpenIntent {
    static var title: LocalizedStringResource = "Talk to Magican"
    static var description = IntentDescription(
        "Starts talking immediately, then keeps Magican available for wake-word follow-ups."
    )

    @Parameter(title: "Destination")
    var target: AmbientControlDestination

    init() {
        target = .listeningWindow
    }

    func perform() async throws -> some IntentResult {
        SharedActions.setPending(SharedActions.PendingAction.ambientArm)
        return .result()
    }
}
