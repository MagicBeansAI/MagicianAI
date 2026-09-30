import Foundation
import UIKit

/// A power condition an armed window has to answer for.
///
/// Two cases rather than one boolean because each one is something different for
/// the user to *do*, and the two are answered differently: the battery floor
/// refuses a window outright, and Low Power Mode only warns. An armed window is
/// exactly the surface where a reason with no action attached is worthless — it is
/// read on the Dynamic Island, with the app not on screen.
enum AmbientPowerBlock: Equatable {
    /// Low Power Mode is on.
    case lowPowerMode

    /// The battery is below the floor and not charging.
    case batteryLow

    /// Shown on the orb when a LIVE window is ended by this.
    ///
    /// Phrased as a consequence rather than a fault, matching
    /// `AmbientYieldReason`: the window ended because of something the user (or
    /// their battery) did, and saying which makes re-arming an obvious next step
    /// instead of a guess.
    var endedReason: String {
        switch self {
        case .lowPowerMode: return "Low Power Mode turned on, so Magican stopped listening."
        case .batteryLow: return "Battery is low, so Magican stopped listening."
        }
    }

    /// Shown in the app when ARMING is refused by this.
    ///
    /// Deliberately different wording from `endedReason`, and derived from the same
    /// case so the two cannot drift: nothing has stopped, so "stopped listening"
    /// would be a lie, and this surface is one the user is looking at — it can
    /// afford to name the fix.
    ///
    /// **Reachable for `.batteryLow` only.** Low Power Mode no longer refuses a
    /// window — see `admission` — so its string here is written honestly rather
    /// than left to a `fatalError` or an optional: an exhaustive `switch` costs one
    /// line and cannot become a crash on the one surface whose whole job is
    /// reporting whether a microphone is live.
    var refusal: String {
        switch self {
        case .lowPowerMode:
            return "Low Power Mode is on. Listening will use more battery than usual."
        case .batteryLow:
            return "Battery is below 20%. Plug Magican in to listen hands-free."
        }
    }

    /// Carried for the WHOLE window when a window is allowed to open in spite of
    /// this — the orb's caption and the in-app bar.
    ///
    /// Short, because it shares a caption line with nothing else and the Dynamic
    /// Island's expanded presentation is not wide. The warning is the entire
    /// justification for arming anyway, so it has to be present for as long as the
    /// window is, not for a moment at the start.
    var warning: String {
        switch self {
        case .lowPowerMode: return "Low Power Mode — higher battery use"
        case .batteryLow: return "Battery low"
        }
    }
}

/// Whether a window may open, given the power conditions at that instant.
enum AmbientPowerAdmission: Equatable {
    case allowed
    /// Open it, and carry the reason for the whole window.
    case allowedWithWarning(AmbientPowerBlock)
    /// Do not open it.
    case refused(AmbientPowerBlock)
}

/// The battery rails from design §9, as pure decisions plus the notifications that
/// ask for them again.
///
/// **Split from `AmbientController` because the decisions are the part with wrong
/// answers that ship silently.** `UIDevice.batteryLevel` is `-1` whenever battery
/// monitoring is off, has just been switched on, or is unavailable — which is every
/// read in the simulator and the first read on a device — and `-1 < 0.20` is true.
/// A rail written as the obvious comparison refuses every window it is asked about,
/// immediately, and the symptom on a device is a feature that "sometimes doesn't
/// arm".
@MainActor
final class AmbientPowerMonitor {

    /// Design §9's floor. Named so the number appears once.
    nonisolated static let batteryFloor: Float = 0.20

    /// Everything the two decisions below are made from, as one value so a test can
    /// place a window on any battery it likes without contriving device state.
    struct Readings: Equatable {
        var lowPowerMode: Bool
        var batteryLevel: Float
        var isCharging: Bool

        /// A phone with nothing to complain about. Named because it is what most
        /// tests want, and spelling it per case buries the one field that varies.
        static let healthy = Readings(lowPowerMode: false, batteryLevel: 1, isCharging: false)
    }

    /// Whether a window may OPEN on these readings.
    ///
    /// **Low Power Mode warns rather than refuses.** Refusing makes ambient mode
    /// unusable for anyone who lives in Low Power Mode — it is sticky, often on for
    /// days — and the battery cost is the user's to accept. So the window opens and
    /// says so, for as long as it is open. The corollary is recorded rather than
    /// hidden: a window armed while Low Power Mode is *already* on will never hear
    /// from either notification (both report a change), so it runs to its leash.
    /// **An informed window running its leash is not the same failure as a silent
    /// one** — which is exactly why `AmbientPowerBlock.warning` has to survive the
    /// whole window rather than the first moment of it.
    ///
    /// **The battery floor still refuses**, unchanged. A window opened at 8% is a
    /// microphone that will outlive the phone, and unlike Low Power Mode there is
    /// nothing informative to say about it that the user can act on while it runs.
    ///
    /// Checked in that order because refusing is the stronger answer: a phone both
    /// in Low Power Mode and below the floor is refused, not warned.
    ///
    /// **Charging is exempt from the floor, and that is a decision.** Design §9 says
    /// "battery < 20%", and read literally that ends the window of a phone sitting
    /// on a charger at 15% and climbing — for a feature whose whole premise is a
    /// phone the user is not holding, which is very often a phone that is plugged
    /// in. The rail exists to stop an armed microphone draining a battery towards
    /// nothing; a battery that is filling is not that.
    ///
    /// A level of `-1` (see the type comment) reads as "unknown", and unknown is
    /// allowed: the safe direction for a *refusal* rail is not to fire, and the
    /// alternative — treating unknown as empty — refuses every window in the
    /// simulator and the first moment of every window on a device.
    nonisolated static func admission(_ readings: Readings) -> AmbientPowerAdmission {
        if batteryIsBelowFloor(readings) { return .refused(.batteryLow) }
        if readings.lowPowerMode { return .allowedWithWarning(.lowPowerMode) }
        return .allowed
    }

    /// Whether a LIVE window has to end on these readings.
    ///
    /// **Low Power Mode is a TRANSITION here, not a level, and the asymmetry with
    /// `admission` is the point.** A window that opened knowing Low Power Mode was
    /// on has already had that conversation with the user; ending it the moment any
    /// unrelated notification fires — a battery level tick, a charger plugged in —
    /// would make the warning a lie and the feature unusable for exactly the people
    /// the warning was written for. What still ends a window is the user turning Low
    /// Power Mode **on while it runs**, which is a fresh instruction rather than a
    /// standing condition.
    ///
    /// `lowPowerModeWasOn` is the last value the monitor observed, not the value at
    /// arm: a window armed in Low Power Mode that then sees it turned off and on
    /// again ends on the second transition, because by then the user has asked for
    /// it twice.
    ///
    /// The battery floor stays a LEVEL, deliberately. There is no transition to wait
    /// for — a battery only crosses the floor downwards on its own — and a window
    /// admitted above the floor that has since fallen below it is precisely what
    /// this rail exists to end.
    nonisolated static func liveBlock(_ readings: Readings, lowPowerModeWasOn: Bool) -> AmbientPowerBlock? {
        if batteryIsBelowFloor(readings) { return .batteryLow }
        if readings.lowPowerMode, !lowPowerModeWasOn { return .lowPowerMode }
        return nil
    }

    private nonisolated static func batteryIsBelowFloor(_ readings: Readings) -> Bool {
        guard readings.batteryLevel >= 0 else { return false }
        guard !readings.isCharging else { return false }
        return readings.batteryLevel < batteryFloor
    }

    /// The live readings, injected as one closure. Production reads `UIDevice` and
    /// `ProcessInfo`; read at the moment a rail acts rather than captured when it
    /// was armed — see `evaluate`.
    var readings: () -> Readings = {
        let device = UIDevice.current
        return Readings(
            lowPowerMode: ProcessInfo.processInfo.isLowPowerModeEnabled,
            batteryLevel: device.batteryLevel,
            isCharging: device.batteryState == .charging || device.batteryState == .full
        )
    }

    /// What a window opening right now would be allowed to do.
    func admission() -> AmbientPowerAdmission {
        Self.admission(readings())
    }

    /// The last Low Power Mode value observed, which is what makes the live rail a
    /// transition. Seeded at `start`, so a window armed in Low Power Mode is not
    /// ended by the condition it was armed under.
    private var lowPowerModeWasOn = false

    private var onBlocked: (@MainActor (AmbientPowerBlock) -> Void)?
    private var isObserving = false

    /// Start watching, and answer the question once immediately.
    ///
    /// The immediate answer covers a battery that fell below the floor between
    /// `arm`'s admission check and this registration. It cannot fire for Low Power
    /// Mode, because `lowPowerModeWasOn` is seeded from the same reading — which is
    /// the whole of "a warned window is not then ended by the thing it was warned
    /// about".
    func start(onBlocked: @escaping @MainActor (AmbientPowerBlock) -> Void) {
        self.onBlocked = onBlocked
        lowPowerModeWasOn = readings().lowPowerMode
        if !isObserving {
            isObserving = true
            // A global switch, and ambient is the only feature in the app that
            // touches it — nothing else reads `batteryLevel`. Turned off again in
            // `stop()` so a window that is over stops the polling the system does
            // on our behalf.
            UIDevice.current.isBatteryMonitoringEnabled = true
            let center = NotificationCenter.default
            center.addObserver(
                self,
                selector: #selector(handle),
                name: .NSProcessInfoPowerStateDidChange,
                object: nil
            )
            center.addObserver(
                self,
                selector: #selector(handle),
                name: UIDevice.batteryLevelDidChangeNotification,
                object: nil
            )
            center.addObserver(
                self,
                selector: #selector(handle),
                name: UIDevice.batteryStateDidChangeNotification,
                object: nil
            )
        }
        evaluate()
    }

    func stop() {
        onBlocked = nil
        guard isObserving else { return }
        isObserving = false
        NotificationCenter.default.removeObserver(self, name: .NSProcessInfoPowerStateDidChange, object: nil)
        NotificationCenter.default.removeObserver(self, name: UIDevice.batteryLevelDidChangeNotification, object: nil)
        NotificationCenter.default.removeObserver(self, name: UIDevice.batteryStateDidChangeNotification, object: nil)
        UIDevice.current.isBatteryMonitoringEnabled = false
    }

    /// `nonisolated`, and the hop is made rather than assumed: the two `UIDevice`
    /// notifications are posted on the main thread but
    /// `NSProcessInfoPowerStateDidChange` is not documented to be, and what this
    /// leads to is a disarm — main-actor state driving a live microphone. Same shape
    /// as every handler in `AmbientMicEngine`, for the same reason.
    @objc nonisolated private func handle() {
        Task { @MainActor [weak self] in self?.evaluate() }
    }

    /// Ask the question again, now.
    ///
    /// **Nothing about the notification is used, deliberately.** All three carry no
    /// payload worth reading, and the answer is a property of the present rather
    /// than of the event: a level change that crossed the floor and a power-state
    /// change that turned Low Power Mode *off* both arrive here, and re-deriving
    /// the verdict is what makes the second one a no-op instead of a disarm. Same
    /// reason `AmbientMicEngine.handleDidBecomeActive` is not generation-checked —
    /// it asks a fresh question rather than delivering an old answer.
    ///
    /// The observed Low Power Mode value is recorded AFTER the decision, because
    /// recording it first would erase the transition the decision is made from.
    private func evaluate() {
        guard onBlocked != nil else { return }
        let current = readings()
        let block = Self.liveBlock(current, lowPowerModeWasOn: lowPowerModeWasOn)
        lowPowerModeWasOn = current.lowPowerMode
        guard let block else { return }
        onBlocked?(block)
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }
}
