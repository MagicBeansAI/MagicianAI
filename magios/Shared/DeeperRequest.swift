import Foundation

/// "Explain this deeper" — turning one storyboard step into a follow-up ask.
///
/// A direct port of `ui/unified-ui/src/lib/tutor/deeperRequest.ts`, kept
/// word-for-word on purpose. The storyboard contract's "a second explanation
/// must change representation, not volume" rule is written against this
/// phrasing, so a platform that asks differently gets a differently-shaped
/// answer for no reason the user could ever see.
public enum DeeperRequest {
    public struct Step {
        public let revealId: String?
        public let label: String?
        /// What was already said, so the retry can differ from it.
        public let narration: String?

        public init(revealId: String? = nil, label: String? = nil, narration: String? = nil) {
            self.revealId = revealId
            self.label = label
            self.narration = narration
        }
    }

    public struct Composed {
        public let sessionId: String
        public let prompt: String
    }

    /// Whether the control can be offered. **Only a session is required.**
    ///
    /// Naming the step sharpens the ask; it does not gate it. A learner can
    /// want more detail at any moment, and the worst case of showing this
    /// button is that someone clicks it and gets more explanation.
    public static func canRequest(step: Step?, sessionId: String?) -> Bool {
        guard let sessionId, !sessionId.trimmingCharacters(in: .whitespaces).isEmpty else {
            return false
        }
        return true
    }

    /// Collapse whitespace so a multi-line narration reads as one quoted sentence.
    private static func flatten(_ text: String) -> String {
        text.split(whereSeparator: { $0.isWhitespace })
            .joined(separator: " ")
            .trimmingCharacters(in: .whitespaces)
    }

    /// The prompt, carrying three load-bearing things: `@tutor` because the
    /// rail is chosen by invoke word; the step's label so the model decomposes
    /// THAT milestone rather than re-teaching from the top; and the narration
    /// already spoken, which it can only avoid repeating if it can see it.
    public static func buildPrompt(step: Step) -> String {
        let label = step.label?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let narration = step.narration?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let named = label.isEmpty
            ? "the part you just explained" : "the step \"\(flatten(label))\""

        var parts = ["@tutor go deeper on \(named)."]
        if !narration.isEmpty {
            parts.append("So far you explained it as: \"\(flatten(narration))\".")
        }
        parts.append(
            "That was not enough. Break this one step into its sub-steps and draw the "
                + "intermediate stages you skipped, rather than restating the same idea in more words. "
                + "Show it — a figure changing, the quantity being matched, the region being shaded — "
                + "and keep the rest of the lesson as it was."
        )
        return parts.joined(separator: " ")
    }

    /// The request to send, or `nil` when there is no session. `nil`
    /// rather than a throw keeps the caller a plain button action: the control
    /// is hidden under the same condition, so `nil` means the two disagreed and
    /// doing nothing is the honest response.
    public static func compose(step: Step?, sessionId: String?) -> Composed? {
        guard canRequest(step: step, sessionId: sessionId), let sessionId else { return nil }
        return Composed(
            sessionId: sessionId.trimmingCharacters(in: .whitespaces),
            prompt: buildPrompt(step: step ?? Step())
        )
    }
}
