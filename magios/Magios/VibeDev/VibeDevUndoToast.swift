import SwiftUI
import UIKit

/// The toast above the bottom bar: a message, Undo while an action waits,
/// and a bar running down the undo window.
struct VibeDevUndoToast: View {
    @ObservedObject var center: VibeDevUndoCenter
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var progress: CGFloat = 1

    var body: some View {
        if let toast = center.toast {
            HStack(spacing: 10) {
                if toast.isError {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(theme.dangerColor)
                }
                Text(toast.message)
                    .font(.themed(13, weight: .semibold))
                    .foregroundStyle(theme.backgroundColor)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if toast.undoable {
                    Button("Undo") { center.undo() }
                        .font(.themed(13, weight: .heavy))
                        .foregroundStyle(theme.accentColor)
                        .brightness(theme.isDark ? 0 : 0.25)
                        .frame(minWidth: Lively.tapMin, minHeight: 32)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 11)
            .background(
                RoundedRectangle(cornerRadius: 12, style: .continuous).fill(theme.textColor)
            )
            .overlay(alignment: .bottomLeading) {
                if toast.undoable {
                    GeometryReader { proxy in
                        Capsule()
                            .fill(theme.accentColor)
                            .frame(width: proxy.size.width * progress, height: 3)
                            .frame(maxHeight: .infinity, alignment: .bottom)
                    }
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .allowsHitTesting(false)
                }
            }
            .padding(.horizontal, 14)
            .id(toast.id)
            .transition(.move(edge: .bottom).combined(with: .opacity))
            .onAppear {
                progress = 1
                guard toast.undoable, !reduceMotion else { return }
                withAnimation(.linear(duration: toast.duration)) { progress = 0 }
            }
            .accessibilityElement(children: .contain)
            .accessibilityAddTraits(.updatesFrequently)
            .onAppear { UIAccessibility.post(notification: .announcement, argument: toast.message) }
        }
    }
}
