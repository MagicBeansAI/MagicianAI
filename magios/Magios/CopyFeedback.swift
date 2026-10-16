import SwiftUI
import UIKit

/// What a tap copies: a task's full id or a file's path (web parity: a short
/// id copies the whole id, a file name copies its absolute path).
enum CopyKind: Equatable {
    case taskId
    case path

    /// The toast / notice after a copy.
    var notice: String {
        switch self {
        case .taskId: return "Copied task id"
        case .path: return "Copied path"
        }
    }

    /// The VoiceOver hint on a copyable label.
    var hint: String {
        switch self {
        case .taskId: return "Double-tap to copy the full id"
        case .path: return "Double-tap to copy the full path"
        }
    }
}

/// The shared copy feedback: the tapped label reads "Copied ✓" for a moment
/// (a crossfade, instant under Reduce Motion), a light haptic, a "Copied"
/// announcement, and the surface's own toast with `CopyKind.notice`.
enum CopyFeedback {
    static let copiedLabel = "Copied ✓"
    static let announcement = "Copied"
    /// How long the label reads "Copied ✓".
    static let revertAfter: TimeInterval = 1.2

    /// Writes `value` (trimmed) through `write` and returns the notice, or
    /// returns nil and writes nothing when there is no value.
    @discardableResult
    static func copy(_ value: String?, kind: CopyKind,
                     write: (String) -> Void = { UIPasteboard.general.string = $0 }) -> String? {
        let text = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !text.isEmpty else { return nil }
        write(text)
        return kind.notice
    }

    /// The label swap's animation: none under Reduce Motion.
    static func animation(reduceMotion: Bool) -> Animation? {
        reduceMotion ? nil : .easeInOut(duration: 0.18)
    }
}

/// A button that copies `value` on tap with `CopyFeedback`. `label` receives
/// whether the label should read "Copied ✓" now (see `CopiedSwap`). Style it
/// with `.buttonStyle(...)` like any button.
struct CopyOnTap<Label: View>: View {
    let value: String?
    let kind: CopyKind
    /// Called with the notice after a copy (the surface's toast).
    var onCopied: (String) -> Void = { _ in }
    @ViewBuilder let label: (Bool) -> Label

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var copied = false
    @State private var token = 0

    var body: some View {
        Button(action: copy) { label(copied) }
            .disabled(value?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
            .accessibilityHint(kind.hint)
    }

    private func copy() {
        guard let notice = CopyFeedback.copy(value, kind: kind) else { return }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        UIAccessibility.post(notification: .announcement, argument: CopyFeedback.announcement)
        token &+= 1
        let current = token
        withAnimation(CopyFeedback.animation(reduceMotion: reduceMotion)) { copied = true }
        onCopied(notice)
        DispatchQueue.main.asyncAfter(deadline: .now() + CopyFeedback.revertAfter) {
            guard current == token else { return }
            withAnimation(CopyFeedback.animation(reduceMotion: reduceMotion)) { copied = false }
        }
    }
}

/// `content` crossfading to "Copied ✓" while `copied`. Both stay laid out, so
/// the label keeps its size.
struct CopiedSwap<Content: View>: View {
    let copied: Bool
    let font: Font
    var color: Color?
    @ViewBuilder let content: () -> Content

    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        ZStack(alignment: .leading) {
            content().opacity(copied ? 0 : 1)
            Text(CopyFeedback.copiedLabel)
                .font(font)
                .foregroundColor(color ?? theme.successColor)
                .lineLimit(1)
                .fixedSize()
                .opacity(copied ? 1 : 0)
                .accessibilityHidden(!copied)
        }
    }
}
