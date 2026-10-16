import AppIntents

/// Compatibility destination retained for shortcuts and installed widgets from
/// releases that exposed a separate one-shot voice launcher.
enum VoiceControlDestination: String, AppEnum {
    case configuredVoice

    static var typeDisplayRepresentation = TypeDisplayRepresentation("Legacy Magican voice destination")
    static var caseDisplayRepresentations: [VoiceControlDestination: DisplayRepresentation] = [
        .configuredVoice: "Configured voice mode"
    ]
}

/// Compatibility-only handoff for previously installed shortcuts and widgets.
/// It is deliberately undiscoverable and is no longer registered as a system
/// control: current surfaces expose one action, `ArmAmbientIntent`, whose first
/// turn starts immediately and whose ambient window remains available after it.
///
/// The legacy intent MUST write the current ambient action too. Leaving its old
/// one-shot Chat action behind would make an already-installed Control Center
/// control look unified after upgrade while still closing after one turn. The
/// intent type and parameter stay stable for App Intents compatibility; only its
/// destination is migrated onto the current lifecycle.
struct StartVoiceIntent: OpenIntent {
    static var title: LocalizedStringResource = "Legacy Magican voice launcher"
    static var description = IntentDescription(
        "Compatibility route for an older Magican voice automation."
    )
    static var isDiscoverable: Bool = false

    @Parameter(title: "Destination")
    var target: VoiceControlDestination

    init() {
        target = .configuredVoice
    }

    func perform() async throws -> some IntentResult {
        SharedActions.setPending(SharedActions.PendingAction.ambientArm)
        return .result()
    }
}
