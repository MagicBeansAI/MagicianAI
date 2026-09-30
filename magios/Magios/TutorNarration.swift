import Foundation

enum TutorNarrationResult: Equatable {
    case completed, cancelled, failed, skipped
}

@MainActor
protocol TutorNarrating: AnyObject {
    func speak(text: String, stepId: String, onStart: @escaping () -> Void) async -> TutorNarrationResult
    func cancel()
}

/// Temporary focus flag used to keep ordinary chat auto-speak from replacing a
/// Tutor step while its narration owns the shared speech synthesizer.
final class TutorAudioFocus {
    static let shared = TutorAudioFocus()

    private let lock = NSLock()
    private var owners: Set<UUID> = []

    private init() {}

    var isActive: Bool {
        lock.lock()
        defer { lock.unlock() }
        return !owners.isEmpty
    }

    func acquire() -> UUID {
        let owner = UUID()
        lock.lock()
        owners.insert(owner)
        lock.unlock()
        return owner
    }

    func release(_ owner: UUID) {
        lock.lock()
        owners.remove(owner)
        lock.unlock()
    }
}

@MainActor
final class SystemTutorNarrator: TutorNarrating {
    private let speech: SpeechSynthesizer
    private var focusOwner: UUID?

    init(speech: SpeechSynthesizer = .shared) {
        self.speech = speech
    }

    func speak(text: String, stepId: String, onStart: @escaping () -> Void) async -> TutorNarrationResult {
        guard !Task.isCancelled else { return .cancelled }
        releaseFocus()
        focusOwner = TutorAudioFocus.shared.acquire()
        let result: TutorNarrationResult = await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                speech.speak(
                    text,
                    messageId: "tutor-step:\(stepId)",
                    onStart: onStart
                ) { result in
                    continuation.resume(returning: Self.map(result))
                }
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.cancel() }
        }
        releaseFocus()
        return result
    }

    func cancel() {
        speech.stop()
        releaseFocus()
    }

    private func releaseFocus() {
        guard let focusOwner else { return }
        TutorAudioFocus.shared.release(focusOwner)
        self.focusOwner = nil
    }

    private static func map(_ result: SpeechPlaybackResult) -> TutorNarrationResult {
        switch result {
        case .completed: return .completed
        case .cancelled: return .cancelled
        case .failed: return .failed
        case .skipped: return .skipped
        }
    }
}

/// Serializes a draw step with its narration. Narrated drawings are revealed
/// from the real audio playback-start callback, not when synthesis is queued.
/// The next item waits for both speech and drawing to complete.
@MainActor
final class TutorPlaybackCoordinator {
    typealias Sleeper = (Double) async -> Void

    private let narrator: TutorNarrating
    private let sleepMilliseconds: Sleeper

    init(
        narrator: TutorNarrating,
        sleepMilliseconds: @escaping Sleeper = { milliseconds in
            guard milliseconds > 0 else { return }
            try? await Task.sleep(nanoseconds: UInt64(milliseconds * 1_000_000))
        }
    ) {
        self.narrator = narrator
        self.sleepMilliseconds = sleepMilliseconds
    }

    func play(
        items: [TutorRevealItem],
        fallbackCaption: String?,
        fallbackNarration: String?,
        fallbackWaitForVoice: Bool,
        onReveal: @escaping (TutorRevealItem, String?) -> Void
    ) async {
        var elapsedDelay = 0.0
        for (index, item) in items.enumerated() {
            guard !Task.isCancelled else { return }
            let wait = max(0, item.delayMs - elapsedDelay)
            await sleepMilliseconds(wait)
            elapsedDelay = item.delayMs
            guard !Task.isCancelled else { return }

            let caption = item.shape.caption ?? fallbackCaption
            let narration = narrationText(
                for: item.shape,
                caption: caption,
                fallbackNarration: index == 0 ? fallbackNarration : nil,
                fallbackWaitForVoice: index == 0 && fallbackWaitForVoice
            )
            var revealStartedAt: Date?
            var didReveal = false
            let reveal = {
                guard !didReveal else { return }
                didReveal = true
                revealStartedAt = Date()
                onReveal(item, caption)
            }

            if let narration {
                let stepId = item.shape.storyboardStepId ?? item.id.uuidString
                let result = await narrator.speak(text: narration, stepId: stepId, onStart: reveal)
                if !didReveal && result != .cancelled { reveal() }
                guard result != .cancelled, !Task.isCancelled else { return }
            } else {
                reveal()
            }

            if let revealStartedAt {
                let elapsedDrawingMs = Date().timeIntervalSince(revealStartedAt) * 1_000
                await sleepMilliseconds(max(0, item.durationMs - elapsedDrawingMs))
            }
        }
    }

    func cancel() { narrator.cancel() }

    private func narrationText(
        for shape: TutorShape,
        caption: String?,
        fallbackNarration: String?,
        fallbackWaitForVoice: Bool
    ) -> String? {
        if let narration = shape.narration?.trimmingCharacters(in: .whitespacesAndNewlines),
           !narration.isEmpty {
            return narration
        }
        if let fallbackNarration = fallbackNarration?.trimmingCharacters(in: .whitespacesAndNewlines),
           !fallbackNarration.isEmpty {
            return fallbackNarration
        }
        if shape.waitForVoice == true || fallbackWaitForVoice,
           let caption = caption?.trimmingCharacters(in: .whitespacesAndNewlines),
           !caption.isEmpty {
            return caption
        }
        return nil
    }
}
