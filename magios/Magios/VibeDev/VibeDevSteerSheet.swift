import SwiftUI

/// Steer a live run with one line (typed or dictated).
struct VibeDevSteerSheet: View {
    let context: VibeDevContext
    let liveTaskID: String
    let name: String
    let onDone: () -> Void

    @StateObject private var dictation = DictationController()
    @ObservedObject private var theme = ThemeManager.shared
    @State private var text = ""
    @State private var sending = false
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Steer \(name)")
                .font(.themedDisplay(18, weight: .semibold))
                .foregroundStyle(theme.textColor)
                .padding(.top, 18)
            Text("The message reaches the run during its current turn.")
                .font(.themed(12))
                .foregroundStyle(theme.secondaryTextColor)
            TaskDetailAskSection(taskId: liveTaskID, panelRows: [])
            VibeDevPromptField(placeholder: "Use Postgres, not SQLite", text: $text, singleLine: true,
                               dictation: dictation)
            if let error {
                Text(error).font(.themed(13)).foregroundStyle(theme.dangerColor)
            }
            HStack(spacing: 8) {
                Button("Cancel") { dismiss() }
                    .buttonStyle(VibeDevButtonStyle(style: .plain, expand: true, large: true))
                Button(action: send) {
                    if sending { ProgressView().tint(theme.onAccentColor) } else { Text("Send") }
                }
                .buttonStyle(VibeDevButtonStyle(style: .primary, expand: true, large: true))
                .disabled(sending || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .background(theme.cardColor.ignoresSafeArea())
        .onDisappear { if dictation.isRecording { dictation.finishLive { _ in } } }
    }

    private func send() {
        sending = true
        error = nil
        let actions = VibeDevActions(context: context)
        Task { @MainActor in
            defer { sending = false }
            if let message = await actions.steer(liveTaskID: liveTaskID, name: name, message: text) {
                error = message
            } else {
                onDone()
            }
        }
    }
}
