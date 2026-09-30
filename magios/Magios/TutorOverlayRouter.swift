import SwiftUI
import UIKit

/// Actual iOS secure-screen state. Protected-data availability transitions when
/// the device locks/unlocks; app lifecycle and scene visibility are deliberately
/// not used because backgrounding an app is not the same thing as locking it.
enum DeviceScreenLock {
    static var isLocked: Bool { !UIApplication.shared.isProtectedDataAvailable }

    static func message(for feature: TutorInvoke.VoiceFeature) -> String {
        feature == .appCopilot
            ? "Please unlock your screen to use App Copilot."
            : "Please unlock your screen to use Tutor."
    }
}

@MainActor
final class TutorOverlayRouter: NSObject, ObservableObject {
    static let shared = TutorOverlayRouter()

    enum VoiceAdmissionResult: Equatable {
        case admitted
        case locked
        case invalidated
    }

    struct Request: Identifiable {
        let id = UUID()
        /// nil ⇒ blackboard (source-free); non-nil ⇒ screen_overlay on the image.
        let screenshot: UIImage?
        let question: String
        let canvasMode: TutorCanvasMode
        /// Voice handoffs begin as soon as the overlay is visible; keyboard and
        /// screenshot entry keep the explicit Ask Tutor affordance.
        let autoStart: Bool
        /// One-shot proof that this voice presentation was admitted while
        /// protected data was available. A lock notification invalidates it;
        /// unlocking never recreates it.
        let voiceAdmissionID: UUID?
    }

    @Published var request: Request? {
        didSet {
            // SwiftUI owns the fullScreenCover binding and may clear it
            // directly on an interactive dismissal. Retire that admission too
            // so abandoned presentations cannot accumulate or be consumed.
            if let prior = oldValue?.voiceAdmissionID,
               prior != request?.voiceAdmissionID {
                pendingVoiceAdmissions.remove(prior)
            }
        }
    }

    private let notificationCenter: NotificationCenter
    private let screenIsLocked: () -> Bool
    private let speaker: (String) -> Void
    private let ambientRail: AmbientRail
    private var pendingVoiceAdmissions: Set<UUID> = []

    init(
        notificationCenter: NotificationCenter = .default,
        screenIsLocked: @escaping () -> Bool = { DeviceScreenLock.isLocked },
        speaker: @escaping (String) -> Void = { message in
            SpeechSynthesizer.shared.speak(message, messageId: "guided-flow-screen-gate")
        },
        ambientRail: AmbientRail = .live
    ) {
        self.notificationCenter = notificationCenter
        self.screenIsLocked = screenIsLocked
        self.speaker = speaker
        self.ambientRail = ambientRail
        super.init()
        // UIApplication posts protected-data lifecycle notifications on the
        // main thread. Selector delivery is synchronous: a `will lock` edge
        // invalidates admission before a queued SwiftUI `.task` can consume it.
        notificationCenter.addObserver(
            self,
            selector: #selector(protectedDataWillBecomeUnavailable(_:)),
            name: UIApplication.protectedDataWillBecomeUnavailableNotification,
            object: nil
        )
    }

    deinit {
        notificationCenter.removeObserver(self)
    }

    @objc private func protectedDataWillBecomeUnavailable(_ notification: Notification) {
        _ = notification
        invalidatePendingVoiceAdmissions(announce: true)
    }

    private func replaceRequest(with next: Request?) {
        if let prior = request?.voiceAdmissionID {
            pendingVoiceAdmissions.remove(prior)
        }
        request = next
    }

    /// Screenshot entry (graduation-cap picker + Share extension) — always screen_overlay.
    func present(screenshot: UIImage, question: String = "") {
        replaceRequest(with: Request(
            screenshot: screenshot,
            question: question,
            canvasMode: .screenOverlay,
            autoStart: false,
            voiceAdmissionID: nil
        ))
    }

    /// Composer `@tutor` entry (web parity): a staged image ⇒ screen_overlay on it,
    /// no image ⇒ blackboard.
    func present(question: String, image: UIImage?, autoStart: Bool = false) {
        var admissionID: UUID?
        if autoStart {
            guard !screenIsLocked() else {
                replaceRequest(with: nil)
                speaker(DeviceScreenLock.message(for: .tutor))
                return
            }
            // A voice Tutor takeover is a foreground product handoff, not the
            // end of one ambient conversation followed by another wake. End the
            // entire armed window before Tutor claims narration; otherwise the
            // window's VoiceCallAudioFocus token makes every Tutor step skip.
            if ambientRail.windowIsLive() {
                ambientRail.yield(AmbientYieldReason.tutorStarted)
            }
            let id = UUID()
            pendingVoiceAdmissions.insert(id)
            admissionID = id
        }
        replaceRequest(with: Request(
            screenshot: image,
            question: question,
            canvasMode: TutorInvoke.mode(hasImage: image != nil),
            autoStart: autoStart,
            voiceAdmissionID: admissionID
        ))
    }

    /// Consume the original lock admission immediately before auto-start. The
    /// token is one-shot: a lock/unlock cycle cannot cause a queued cover to run.
    func consumeVoiceAdmission(_ id: UUID?) -> VoiceAdmissionResult {
        guard let id, pendingVoiceAdmissions.remove(id) != nil else {
            return .invalidated
        }
        return screenIsLocked() ? .locked : .admitted
    }

    func invalidatePendingVoiceAdmissions(announce: Bool) {
        guard let id = request?.voiceAdmissionID,
              pendingVoiceAdmissions.remove(id) != nil else { return }
        replaceRequest(with: nil)
        if announce { speaker(DeviceScreenLock.message(for: .tutor)) }
    }

    @discardableResult
    func present(token: String) -> Bool {
        guard let (handoff, data) = TutorOverlayInbox.load(token: token),
              let image = UIImage(data: data) else { return false }
        present(screenshot: image, question: handoff.question)
        return true
    }
}
